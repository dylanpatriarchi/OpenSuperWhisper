//! Deterministic, low-latency corrections for Italian dictation.
//!
//! This runs on **every** dictation, so it does no model call and no lookup
//! beyond a fixed table — it is plain string work, measured in microseconds.
//!
//! **Only always-true rules live here.** Every rule must be correct in every
//! context, with no exceptions. Anything needing context to disambiguate —
//! `e`/`è`, `si`/`sì`, `la`/`là`, `da`/`dà`, `ne`/`né`, `se`/`sé` — is
//! deliberately excluded: both spellings are valid Italian words, so a
//! context-free rule would silently change meaning. Those belong to the
//! optional LLM reformulation layer, not here.
//!
//! Note that a good multilingual model (Whisper large-v3-turbo) already writes
//! correctly accented Italian. The value of this pass is as cheap insurance for
//! lighter/faster engines, which are much more likely to drop accents.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// Words whose unaccented spelling is **not** a valid Italian word, so
/// restoring the accent can never be wrong.
///
/// Deliberately absent, because the unaccented form is itself a word:
/// `pero` (pear tree) → `però`, `meta` (goal) → `metà`,
/// `giacche` (jackets) → `giacché`, `subito` (immediately) → `subìto`,
/// `te` (you — a very common pronoun) → `tè` (tea),
/// `eta` (the Greek letter) → `età` (age).
static UNAMBIGUOUS_ACCENTS: &[(&str, &str)] = &[
    // Conjunctions in -ché
    ("perche", "perché"),
    ("poiche", "poiché"),
    ("benche", "benché"),
    ("affinche", "affinché"),
    ("finche", "finché"),
    ("anziche", "anziché"),
    ("nonche", "nonché"),
    ("cosicche", "cosicché"),
    ("sicche", "sicché"),
    ("dacche", "dacché"),
    ("fuorche", "fuorché"),
    ("granche", "granché"),
    // Common adverbs and verb forms
    ("piu", "più"),
    ("puo", "può"),
    ("gia", "già"),
    ("cioe", "cioè"),
    ("percio", "perciò"),
    ("cosi", "così"),
    ("laggiu", "laggiù"),
    ("quaggiu", "quaggiù"),
    ("lassu", "lassù"),
    ("quassu", "quassù"),
    // Nouns in -tà / -tù
    ("citta", "città"),
    ("universita", "università"),
    ("societa", "società"),
    ("qualita", "qualità"),
    ("realta", "realtà"),
    ("liberta", "libertà"),
    ("verita", "verità"),
    ("novita", "novità"),
    ("attivita", "attività"),
    ("possibilita", "possibilità"),
    ("necessita", "necessità"),
    ("difficolta", "difficoltà"),
    ("identita", "identità"),
    ("virtu", "virtù"),
    ("gioventu", "gioventù"),
    ("tribu", "tribù"),
    // Other nouns
    ("caffe", "caffè"),
    // Weekdays
    ("lunedi", "lunedì"),
    ("martedi", "martedì"),
    ("mercoledi", "mercoledì"),
    ("giovedi", "giovedì"),
    ("venerdi", "venerdì"),
];

fn unambiguous_accents() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| UNAMBIGUOUS_ACCENTS.iter().copied().collect())
}

/// Spellings that are simply never correct in Italian, whatever the context.
///
/// The Swift original writes the `qual'` rule with a lookahead
/// (`\bqual'(?=[eè])` → `"qual "`); the `regex` crate has no lookahead, so it
/// is expressed equivalently with a capture group that re-inserts the vowel.
fn always_wrong_spellings() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            // "pò" does not exist: it is a truncation of "poco", so it takes an apostrophe.
            (r"(?i)\bp(ò)\b", "po'"),
            // "qual" is a truncation, never an elision — it never takes an apostrophe.
            (r"(?i)\bqual'([eè])", "qual $1"),
            // "un" before a masculine word takes no apostrophe. Only masculine
            // targets are listed: "un'altra", "un'ora", "un'amica" are all correct.
            (r"(?i)\bun'(altro|uomo|amico|anno|attimo)\b", "un $1"),
            // "d'accordo" is two words joined by an apostrophe.
            (r"(?i)\bdaccordo\b", "d'accordo"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).unwrap(), replacement))
        .collect()
    })
}

/// Applies every always-true correction. Safe to call on any string;
/// returns the input unchanged when there is nothing to fix.
pub fn correct(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }

    let result = restore_accents(text);
    let result = fix_always_wrong_spellings(&result);
    normalize_whitespace_and_punctuation(&result)
}

// ---- Steps ----------------------------------------------------------------

/// Restores accents only on words from `UNAMBIGUOUS_ACCENTS`, preserving the
/// original capitalisation of the first letter.
fn restore_accents(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut current = String::new();

    for character in text.chars() {
        if character.is_alphabetic() || character == '\'' {
            current.push(character);
        } else {
            result.push_str(&replacement_if_accented(&current));
            current.clear();
            result.push(character);
        }
    }
    result.push_str(&replacement_if_accented(&current));
    result
}

fn replacement_if_accented(word: &str) -> String {
    let accented = match unambiguous_accents().get(word.to_lowercase().as_str()) {
        Some(accented) if !word.is_empty() => *accented,
        _ => return word.to_string(),
    };

    // Keep the caller's capitalisation: "Perche" -> "Perché".
    match word.chars().next() {
        Some(first) if first.is_uppercase() => {
            let mut chars = accented.chars();
            let head: String = chars.next().map(char::to_uppercase).into_iter().flatten().collect();
            head + chars.as_str()
        }
        _ => accented.to_string(),
    }
}

fn fix_always_wrong_spellings(text: &str) -> String {
    let mut result = text.to_string();
    for (regex, replacement) in always_wrong_spellings() {
        result = regex.replace_all(&result, *replacement).into_owned();
    }
    result
}

/// Collapses repeated spaces and removes spaces before punctuation, which is
/// always wrong in Italian typography. Does not touch spacing *after*
/// punctuation: that would corrupt decimals ("1,5"), times and URLs.
fn normalize_whitespace_and_punctuation(text: &str) -> String {
    static SPACE_BEFORE_PUNCTUATION: OnceLock<Regex> = OnceLock::new();
    static REPEATED_SPACES: OnceLock<Regex> = OnceLock::new();

    let space_before_punctuation =
        SPACE_BEFORE_PUNCTUATION.get_or_init(|| Regex::new(r"[ \t]+([,.;:!?])").unwrap());
    let repeated_spaces = REPEATED_SPACES.get_or_init(|| Regex::new(r"[ \t]{2,}").unwrap());

    let result = space_before_punctuation.replace_all(text, "$1");
    repeated_spaces.replace_all(&result, " ").into_owned()
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::correct;

    // The point of "conservative": ambiguous words must survive untouched.

    /// Both spellings are valid Italian words, so a context-free rule would
    /// silently change meaning. These must never be rewritten here.
    #[test]
    fn ambiguous_words_are_never_touched() {
        let untouched = [
            "e",       // "e" (and) vs "è" (is)
            "si",      // "si" (oneself) vs "sì" (yes)
            "la",      // "la" (the) vs "là" (there)
            "da",      // "da" (from) vs "dà" (gives)
            "ne",      // "ne" (of it) vs "né" (nor)
            "se",      // "se" (if) vs "sé" (self)
            "pero",    // "pero" (pear tree) vs "però" (but)
            "meta",    // "meta" (goal) vs "metà" (half)
            "giacche", // "giacche" (jackets) vs "giacché" (since)
            "subito",  // "subito" (immediately) vs "subìto" (undergone)
            "te",      // "te" (you, pronoun) vs "tè" (tea)
            "eta",     // "eta" (Greek letter) vs "età" (age)
        ];

        for word in untouched {
            let sentence = format!("questo {word} qui");
            assert_eq!(
                correct(&sentence),
                sentence,
                "'{word}' is ambiguous and must be left alone"
            );
        }
    }

    #[test]
    fn ambiguous_words_survive_in_realistic_sentences() {
        // "e" must not become "è" even where a human would write "è".
        let cases = [
            "il pero e in giardino",
            "la meta del lavoro e finita",
            "si e fatto tardi",
        ];
        for sentence in cases {
            assert_eq!(correct(sentence), sentence);
        }
    }

    // Accent restoration (unaccented form is not a word).

    #[test]
    fn restores_unambiguous_accents() {
        assert_eq!(correct("non so perche"), "non so perché");
        assert_eq!(correct("un po piu tardi"), "un po più tardi");
        assert_eq!(correct("si puo fare"), "si può fare");
        assert_eq!(correct("e gia pronto"), "e già pronto");
        assert_eq!(correct("vivo in citta"), "vivo in città");
        assert_eq!(correct("ci vediamo lunedi"), "ci vediamo lunedì");
        assert_eq!(correct("cioe non lo so"), "cioè non lo so");
    }

    #[test]
    fn accent_restoration_preserves_capitalisation() {
        assert_eq!(correct("Perche no?"), "Perché no?");
        assert_eq!(correct("Citta di Roma"), "Città di Roma");
    }

    #[test]
    fn already_accented_text_is_unchanged() {
        // What a good model already produces must survive verbatim.
        let good = "Effettivamente te la scrive, ora è proprio una roba un pochino più complicata.";
        assert_eq!(correct(good), good);
    }

    // Spellings that are never correct.

    #[test]
    fn fixes_always_wrong_spellings() {
        assert_eq!(correct("aspetta un pò"), "aspetta un po'");
        assert_eq!(correct("qual'è il problema"), "qual è il problema");
        assert_eq!(correct("è un'altro giorno"), "è un altro giorno");
        assert_eq!(correct("siamo daccordo"), "siamo d'accordo");
    }

    /// The feminine elision is correct and must not be "fixed".
    #[test]
    fn feminine_elision_is_preserved() {
        for text in ["un'altra volta", "un'ora fa", "un'amica mia"] {
            assert_eq!(correct(text), text);
        }
    }

    // Whitespace and punctuation.

    #[test]
    fn removes_space_before_punctuation_and_collapses_runs() {
        assert_eq!(correct("ciao , come stai ?"), "ciao, come stai?");
        assert_eq!(correct("due  spazi"), "due spazi");
    }

    /// Spacing *after* punctuation is left alone on purpose: touching it would
    /// corrupt decimals, times and URLs.
    #[test]
    fn does_not_insert_space_after_punctuation() {
        for text in ["costa 1,5 euro", "alle 10.30", "vai su example.com/pagina"] {
            assert_eq!(correct(text), text);
        }
    }

    // Anglicisms.

    /// English loanwords are normal in spoken Italian and must be transcribed
    /// as spoken — never translated or "corrected".
    #[test]
    fn english_loanwords_are_untouched() {
        let sentence = "sposta la call dopo il meeting, la deadline è il budget review";
        assert_eq!(correct(sentence), sentence);
    }

    // Safety.

    #[test]
    fn empty_and_plain_text_are_safe() {
        assert_eq!(correct(""), "");
        assert_eq!(
            correct("testo normale senza errori"),
            "testo normale senza errori"
        );
    }

    #[test]
    fn is_idempotent() {
        let input = "perche un pò , qual'è  la citta ?";
        let once = correct(input);
        assert_eq!(
            correct(&once),
            once,
            "Running the pass twice must not keep changing the text"
        );
    }
}
