import Foundation
import HuggingFace
import MLXHuggingFace
import MLXLLM
import MLXLMCommon
import Tokenizers

/// Rewrites a raw dictation into what the speaker meant to say, using a local
/// LLM via MLX.
///
/// This is the *second* correction layer and is deliberately separate from
/// ``ItalianTextCorrector``:
///
/// - ``ItalianTextCorrector`` is deterministic, always-on and costs
///   microseconds. It only applies rules that are true in every context.
/// - This layer understands the sentence. It removes spoken self-corrections
///   ("domani alle 10, ah no, alle 10.30" → "domani alle 10.30") and fillers,
///   which no ASR model will ever do — Whisper and Parakeet transcribe
///   faithfully, by design.
///
/// It costs seconds and can be wrong, so it is opt-in, and the caller keeps the
/// raw transcription regardless.
@MainActor
final class ReformulationService: ObservableObject {
    static let shared = ReformulationService()

    @Published private(set) var isLoading = false
    @Published private(set) var isReady = false

    /// Loaded lazily: most users never enable reformulation, and the model is a
    /// multi-gigabyte download we must not fetch on first launch.
    private var container: ModelContainer?
    private var loadTask: Task<ModelContainer, Error>?

    private init() {}

    /// The system prompt. Two things matter for Italian dictation and are easy
    /// to get wrong, so they are stated explicitly:
    ///
    /// - English loanwords are normal in spoken Italian ("meeting", "call",
    ///   "deadline") and must survive verbatim — translating them is a bug.
    /// - The model must not answer, summarise or add anything. It rewrites.
    static let instructions = """
        Sei un correttore di dettature vocali in italiano. Ricevi la trascrizione \
        grezza di qualcuno che parla e la riscrivi in forma pulita.

        Regole:
        - Rimuovi le autocorrezioni del parlato: se chi parla si corregge, tieni \
        SOLO la versione corretta. Esempio: "domani alle 10 non ci sarò, ah no, \
        non è vero, alle 10.30" diventa "domani non ci sarò alle 10.30".
        - Rimuovi le esitazioni e gli intercalari ("ehm", "cioè", "come si dice", \
        "diciamo") quando non aggiungono significato.
        - Correggi punteggiatura, accenti e maiuscole.
        - NON tradurre i termini stranieri: "meeting", "call", "deadline", \
        "budget" restano come sono. Sono normali in italiano parlato.
        - NON aggiungere informazioni, NON rispondere, NON riassumere, NON \
        commentare. Riscrivi soltanto.
        - Mantieni il registro e il tono di chi parla.
        - Se il testo è già pulito, restituiscilo identico.

        Rispondi esclusivamente con il testo riscritto, senza virgolette e senza \
        alcuna premessa.
        """

    /// Rewrites shorter than this are never rejected for length. Below it, the
    /// "3× the input" rule is too tight to mean anything.
    static let shortDictationAllowance = 120

    /// Deterministic decoding: this is a rewriting task, so sampling would only
    /// add variation we do not want.
    private static let generateParameters = GenerateParameters(
        maxTokens: 512,
        temperature: 0.0
    )

    /// Downloads (first time) and loads the model. Safe to call repeatedly —
    /// concurrent callers share one load.
    @discardableResult
    func prepare() async throws -> ModelContainer {
        if let container { return container }

        if let loadTask {
            return try await loadTask.value
        }

        isLoading = true
        let task = Task<ModelContainer, Error> {
            try await #huggingFaceLoadModelContainer(
                configuration: LLMRegistry.gemma4_e2b_it_4bit
            )
        }
        loadTask = task

        defer {
            isLoading = false
            loadTask = nil
        }

        do {
            let loaded = try await task.value
            container = loaded
            isReady = true
            return loaded
        } catch {
            isReady = false
            throw error
        }
    }

    /// Rewrites `text`. Returns the input unchanged if the model produces
    /// nothing usable — a failed reformulation must never lose the dictation.
    func reformulate(_ text: String) async throws -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return text }

        let container = try await prepare()
        let session = ChatSession(
            container,
            instructions: Self.instructions,
            generateParameters: Self.generateParameters
        )

        let response = try await session.respond(to: trimmed)
        return Self.sanitize(response, fallingBackTo: text)
    }

    /// Small models sometimes wrap the answer in quotes or prefix it with
    /// "Testo riscritto:". Strip that, and refuse anything that looks like the
    /// model answered instead of rewriting.
    static func sanitize(_ response: String, fallingBackTo original: String) -> String {
        var cleaned = response.trimmingCharacters(in: .whitespacesAndNewlines)

        for prefix in ["Testo riscritto:", "Testo pulito:", "Riscrittura:", "Output:"] {
            if cleaned.lowercased().hasPrefix(prefix.lowercased()) {
                cleaned = String(cleaned.dropFirst(prefix.count))
                    .trimmingCharacters(in: .whitespacesAndNewlines)
            }
        }

        // Typographic quotes too: models reach for “ ” at least as often as ",
        // and an unstripped pair ends up pasted into the user's document.
        //
        // The opening character must match its own closing partner, and the
        // interior must be free of both. Otherwise a sentence that merely
        // starts and ends with a quotation — «"Vengo" disse, poi "no aspetta"»
        // — looks exactly like a wrapped one, and stripping the ends leaves
        // the user with mangled, unbalanced text.
        let quotePairs: [Character: Character] = [
            "\"": "\"", "\u{201C}": "\u{201D}", "\u{00AB}": "\u{00BB}",
        ]
        if cleaned.count >= 2,
           let first = cleaned.first,
           let closing = quotePairs[first],
           cleaned.last == closing,
           cleaned.dropFirst().dropLast().allSatisfy({ $0 != first && $0 != closing }) {
            cleaned = String(cleaned.dropFirst().dropLast())
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }

        guard !cleaned.isEmpty else { return original }

        // A rewrite is roughly the length of the input. Something far longer is
        // the model having answered the dictation rather than cleaning it.
        //
        // The floor matters: on a two-word dictation, 3× is a handful of
        // characters, and a perfectly good rewrite ("10" → "Sono le dieci.")
        // would be thrown away for being "too long".
        let originalLength = original.trimmingCharacters(in: .whitespacesAndNewlines).count
        let lengthCeiling = max(originalLength * 3, Self.shortDictationAllowance)
        if originalLength > 0, cleaned.count > lengthCeiling {
            return original
        }

        return cleaned
    }
}
