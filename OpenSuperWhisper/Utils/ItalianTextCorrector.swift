import Foundation

/// Deterministic, low-latency corrections for Italian dictation.
///
/// This runs on **every** dictation, so it does no model call and no lookup
/// beyond a fixed table — it is plain string work, measured in microseconds.
///
/// **Only always-true rules live here.** Every rule must be correct in every
/// context, with no exceptions. Anything needing context to disambiguate —
/// `e`/`è`, `si`/`sì`, `la`/`là`, `da`/`dà`, `ne`/`né`, `se`/`sé` — is
/// deliberately excluded: both spellings are valid Italian words, so a
/// context-free rule would silently change meaning. Those belong to the
/// optional LLM reformulation layer, not here.
///
/// Note that a good multilingual model (Whisper large-v3-turbo) already writes
/// correctly accented Italian. The value of this pass is as cheap insurance for
/// lighter/faster engines, which are much more likely to drop accents.
enum ItalianTextCorrector {

    /// Words whose unaccented spelling is **not** a valid Italian word, so
    /// restoring the accent can never be wrong.
    ///
    /// Deliberately absent, because the unaccented form is itself a word:
    /// `pero` (pear tree) → `però`, `meta` (goal) → `metà`,
    /// `giacche` (jackets) → `giacché`, `subito` (immediately) → `subìto`,
    /// `te` (you — a very common pronoun) → `tè` (tea),
    /// `eta` (the Greek letter) → `età` (age).
    static let unambiguousAccents: [String: String] = [
        // Conjunctions in -ché
        "perche": "perché", "poiche": "poiché", "benche": "benché",
        "affinche": "affinché", "finche": "finché", "anziche": "anziché",
        "nonche": "nonché", "cosicche": "cosicché", "sicche": "sicché",
        "dacche": "dacché", "fuorche": "fuorché", "granche": "granché",
        // Common adverbs and verb forms
        "piu": "più", "puo": "può", "gia": "già", "cioe": "cioè",
        "percio": "perciò", "cosi": "così", "laggiu": "laggiù",
        "quaggiu": "quaggiù", "lassu": "lassù", "quassu": "quassù",
        // Nouns in -tà / -tù
        "citta": "città", "universita": "università", "societa": "società",
        "qualita": "qualità", "realta": "realtà", "liberta": "libertà",
        "verita": "verità", "novita": "novità", "attivita": "attività",
        "possibilita": "possibilità", "necessita": "necessità",
        "difficolta": "difficoltà", "identita": "identità",
        "virtu": "virtù", "gioventu": "gioventù", "tribu": "tribù",
        // Other nouns
        "caffe": "caffè",
        // Weekdays
        "lunedi": "lunedì", "martedi": "martedì", "mercoledi": "mercoledì",
        "giovedi": "giovedì", "venerdi": "venerdì",
    ]

    /// Spellings that are simply never correct in Italian, whatever the context.
    private static let alwaysWrongSpellings: [(pattern: String, replacement: String)] = [
        // "pò" does not exist: it is a truncation of "poco", so it takes an apostrophe.
        (#"\bp(ò)\b"#, "po'"),
        // "qual" is a truncation, never an elision — it never takes an apostrophe.
        (#"\bqual'(?=[eè])"#, "qual "),
        // "un" before a masculine word takes no apostrophe. Only masculine
        // targets are listed: "un'altra", "un'ora", "un'amica" are all correct.
        (#"\bun'(altro|uomo|amico|anno|attimo)\b"#, "un $1"),
        // "d'accordo" is two words joined by an apostrophe.
        (#"\bdaccordo\b"#, "d'accordo"),
    ]

    /// Applies every always-true correction. Safe to call on any string;
    /// returns the input unchanged when there is nothing to fix.
    static func correct(_ text: String) -> String {
        guard !text.isEmpty else { return text }

        var result = text
        result = restoreAccents(in: result)
        result = fixAlwaysWrongSpellings(in: result)
        result = normalizeWhitespaceAndPunctuation(in: result)
        return result
    }

    // MARK: - Steps

    /// Restores accents only on words from `unambiguousAccents`, preserving the
    /// original capitalisation of the first letter.
    static func restoreAccents(in text: String) -> String {
        var result = ""
        result.reserveCapacity(text.count)

        var current = ""
        for character in text {
            if character.isLetter || character == "'" {
                current.append(character)
            } else {
                result.append(replacementIfAccented(current))
                current = ""
                result.append(character)
            }
        }
        result.append(replacementIfAccented(current))
        return result
    }

    private static func replacementIfAccented(_ word: String) -> String {
        guard !word.isEmpty,
              let accented = unambiguousAccents[word.lowercased()]
        else { return word }

        // Keep the caller's capitalisation: "Perche" -> "Perché".
        guard let first = word.first, first.isUppercase else { return accented }
        return accented.prefix(1).uppercased() + accented.dropFirst()
    }

    private static func fixAlwaysWrongSpellings(in text: String) -> String {
        var result = text
        for rule in alwaysWrongSpellings {
            guard let regex = try? NSRegularExpression(
                pattern: rule.pattern,
                options: [.caseInsensitive]
            ) else { continue }
            result = regex.stringByReplacingMatches(
                in: result,
                range: NSRange(result.startIndex..., in: result),
                withTemplate: rule.replacement
            )
        }
        return result
    }

    /// Collapses repeated spaces and removes spaces before punctuation, which is
    /// always wrong in Italian typography. Does not touch spacing *after*
    /// punctuation: that would corrupt decimals ("1,5"), times and URLs.
    static func normalizeWhitespaceAndPunctuation(in text: String) -> String {
        var result = text

        if let spaceBeforePunctuation = try? NSRegularExpression(pattern: #"[ \t]+([,.;:!?])"#) {
            result = spaceBeforePunctuation.stringByReplacingMatches(
                in: result,
                range: NSRange(result.startIndex..., in: result),
                withTemplate: "$1"
            )
        }

        if let repeatedSpaces = try? NSRegularExpression(pattern: #"[ \t]{2,}"#) {
            result = repeatedSpaces.stringByReplacingMatches(
                in: result,
                range: NSRange(result.startIndex..., in: result),
                withTemplate: " "
            )
        }

        return result
    }
}
