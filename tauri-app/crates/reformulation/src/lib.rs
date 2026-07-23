//! Pure, model-independent half of the LLM reformulation service, ported 1:1
//! from `OpenSuperWhisper/Engines/ReformulationService.swift`.
//!
//! Reformulation rewrites a raw Italian dictation into what the speaker meant
//! to say. This crate holds the guard between the model's output and the
//! user's text: the system prompt, the decoding constants, and [`sanitize`],
//! which makes sure a failed reformulation never loses the dictation. The
//! llama.cpp engine wiring lives in [`engine`].

pub mod engine;

/// The system prompt. Two things matter for Italian dictation and are easy
/// to get wrong, so they are stated explicitly:
///
/// - English loanwords are normal in spoken Italian ("meeting", "call",
///   "deadline") and must survive verbatim — translating them is a bug.
/// - The model must not answer, summarise or add anything. It rewrites.
///
/// Copied verbatim from `ReformulationService.instructions` (Swift `\`
/// line-continuations join lines without a newline).
pub const INSTRUCTIONS: &str = "Sei un correttore di dettature vocali in italiano. Ricevi la trascrizione grezza di qualcuno che parla e la riscrivi in forma pulita.\n\nRegole:\n- Rimuovi le autocorrezioni del parlato: se chi parla si corregge, tieni SOLO la versione corretta. Esempio: \"domani alle 10 non ci sarò, ah no, non è vero, alle 10.30\" diventa \"domani non ci sarò alle 10.30\".\n- Rimuovi le esitazioni e gli intercalari (\"ehm\", \"cioè\", \"come si dice\", \"diciamo\") quando non aggiungono significato.\n- Correggi punteggiatura, accenti e maiuscole.\n- NON tradurre i termini stranieri: \"meeting\", \"call\", \"deadline\", \"budget\" restano come sono. Sono normali in italiano parlato.\n- NON aggiungere informazioni, NON rispondere, NON riassumere, NON commentare. Riscrivi soltanto.\n- Mantieni il registro e il tono di chi parla.\n- Se il testo è già pulito, restituiscilo identico.\n\nRispondi esclusivamente con il testo riscritto, senza virgolette e senza alcuna premessa.";

/// Rewrites shorter than this are never rejected for length. Below it, the
/// "3× the input" rule is too tight to mean anything.
pub const SHORT_DICTATION_ALLOWANCE: usize = 120;

/// Deterministic decoding: this is a rewriting task, so sampling would only
/// add variation we do not want. `TEMPERATURE` 0.0 is realised in [`engine`]
/// as greedy decoding.
pub const MAX_TOKENS: u32 = 512;
pub const TEMPERATURE: f32 = 0.0;

/// Preambles that small models sometimes prefix the answer with.
const PREAMBLE_PREFIXES: [&str; 4] = [
    "Testo riscritto:",
    "Testo pulito:",
    "Riscrittura:",
    "Output:",
];

/// Quote pairs eligible for unwrapping: ASCII, typographic, guillemets.
const QUOTE_PAIRS: [(char, char); 3] = [('"', '"'), ('\u{201C}', '\u{201D}'), ('\u{00AB}', '\u{00BB}')];

/// Small models sometimes wrap the answer in quotes or prefix it with
/// "Testo riscritto:". Strip that, and refuse anything that looks like the
/// model answered instead of rewriting.
///
/// Returns the `original` unchanged if the response produces nothing usable —
/// a failed reformulation must never lose the dictation.
pub fn sanitize(response: &str, original: &str) -> String {
    let mut cleaned = response.trim();

    for prefix in PREAMBLE_PREFIXES {
        // Swift compares `cleaned.lowercased().hasPrefix(prefix.lowercased())`:
        // a case-insensitive prefix match. The prefixes are pure ASCII, so an
        // ASCII-case-insensitive byte comparison is equivalent (and a match
        // guarantees the boundary at `prefix.len()` is a char boundary).
        if cleaned.len() >= prefix.len()
            && cleaned.is_char_boundary(prefix.len())
            && cleaned[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            cleaned = cleaned[prefix.len()..].trim();
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
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() >= 2 {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        if let Some(&(_, closing)) = QUOTE_PAIRS.iter().find(|&&(open, _)| open == first) {
            if last == closing
                && chars[1..chars.len() - 1]
                    .iter()
                    .all(|&c| c != first && c != closing)
            {
                cleaned = cleaned[first.len_utf8()..cleaned.len() - closing.len_utf8()].trim();
            }
        }
    }

    if cleaned.is_empty() {
        return original.to_string();
    }

    // A rewrite is roughly the length of the input. Something far longer is
    // the model having answered the dictation rather than cleaning it.
    //
    // The floor matters: on a two-word dictation, 3× is a handful of
    // characters, and a perfectly good rewrite ("10" → "Sono le dieci.")
    // would be thrown away for being "too long".
    let original_length = original.trim().chars().count();
    let length_ceiling = (original_length * 3).max(SHORT_DICTATION_ALLOWANCE);
    if original_length > 0 && cleaned.chars().count() > length_ceiling {
        return original.to_string();
    }

    cleaned.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "domani alle 10 non ci sarò, ah no, alle 10.30";

    // Falling back to the raw transcription

    #[test]
    fn empty_response_falls_back_to_the_original() {
        assert_eq!(sanitize("", ORIGINAL), ORIGINAL);
        assert_eq!(sanitize("   \n ", ORIGINAL), ORIGINAL);
    }

    /// A rewrite is roughly as long as its input. Something far longer means the
    /// model answered the dictation instead of cleaning it, which would replace
    /// the user's text with an unrelated reply.
    #[test]
    fn answering_instead_of_rewriting_falls_back_to_the_original() {
        let answer = "Certo, ecco una risposta molto lunga. ".repeat(10);
        assert!(answer.chars().count() > ORIGINAL.chars().count() * 3);
        assert_eq!(sanitize(&answer, ORIGINAL), ORIGINAL);
    }

    #[test]
    fn a_moderately_longer_rewrite_is_accepted() {
        let rewrite = "Domani non ci sarò alle 10.30.";
        assert_eq!(sanitize(rewrite, ORIGINAL), rewrite);
    }

    // Stripping model scaffolding

    #[test]
    fn strips_preamble() {
        assert_eq!(
            sanitize("Testo riscritto: Domani non ci sarò.", ORIGINAL),
            "Domani non ci sarò."
        );
        assert_eq!(
            sanitize("Output: Domani non ci sarò.", ORIGINAL),
            "Domani non ci sarò."
        );
    }

    #[test]
    fn strips_wrapping_quotes() {
        assert_eq!(
            sanitize("\"Domani non ci sarò.\"", ORIGINAL),
            "Domani non ci sarò."
        );
    }

    /// Models reach for typographic quotes at least as often as ASCII ones, and
    /// an unstripped pair is pasted straight into the user's document.
    #[test]
    fn strips_typographic_quotes() {
        for wrapped in [
            "\u{201C}Domani non ci sarò.\u{201D}",
            "\u{00AB}Domani non ci sarò.\u{00BB}",
        ] {
            assert_eq!(
                sanitize(wrapped, ORIGINAL),
                "Domani non ci sarò.",
                "quotes in {wrapped} should have been stripped"
            );
        }
    }

    /// On a very short dictation, "3× the input" is a handful of characters, so
    /// a perfectly good rewrite would be discarded for being too long.
    #[test]
    fn short_dictations_are_not_rejected_for_length() {
        let short = "alle 10";
        let rewrite = "Ci vediamo domani mattina alle 10.";
        assert!(
            rewrite.chars().count() > short.chars().count() * 3,
            "fixture is pointless unless the rewrite really exceeds 3x"
        );
        assert_eq!(sanitize(rewrite, short), rewrite);
    }

    /// The allowance must not become a blank cheque: a genuine answer is still
    /// rejected, even when the input was short.
    #[test]
    fn the_short_allowance_still_rejects_an_answer() {
        let short = "alle 10";
        let answer = "Certo, ecco una risposta lunghissima. ".repeat(10);
        assert_eq!(sanitize(&answer, short), short);
    }

    /// Quotes that are part of the sentence must survive — only a pair wrapping
    /// the *whole* response is scaffolding.
    #[test]
    fn quotes_inside_the_text_are_kept() {
        let quoted = "Mi ha detto \"arrivo\" e poi è sparito.";
        assert_eq!(sanitize(quoted, ORIGINAL), quoted);
    }

    /// A sentence that merely *starts and ends* with a quotation looks exactly
    /// like a wrapped one. Stripping the ends there leaves unbalanced, mangled
    /// text, so these must survive untouched.
    #[test]
    fn sentences_that_open_and_close_with_quotes_are_not_unwrapped() {
        let cases = [
            "\u{201C}Vengo\u{201D} disse lui, poi aggiunse \u{201C}no aspetta\u{201D}",
            "\"Vengo\" disse lui, poi aggiunse \"no aspetta\"",
            "\u{201C}Vengo\u{201D} disse lui, poi aggiunse \"no aspetta\"",
        ];
        for text in cases {
            assert_eq!(
                sanitize(text, ORIGINAL),
                text,
                "two separate quotations must not be read as one wrapping pair"
            );
        }
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            sanitize("\n  Domani non ci sarò.  \n", ORIGINAL),
            "Domani non ci sarò."
        );
    }

    // Anglicisms

    /// English loanwords are normal in spoken Italian; sanitisation must not be
    /// what mangles them.
    #[test]
    fn anglicisms_survive_sanitisation() {
        let text = "Sposta la call dopo il meeting, la deadline è venerdì.";
        assert_eq!(sanitize(text, ORIGINAL), text);
    }

    // The prompt itself

    /// The two rules that are easy to lose in a prompt edit and expensive to
    /// notice: don't translate loanwords, don't answer.
    #[test]
    fn instructions_forbid_translating_and_answering() {
        assert!(INSTRUCTIONS.contains("NON tradurre"));
        assert!(INSTRUCTIONS.contains("NON rispondere"));
    }
}
