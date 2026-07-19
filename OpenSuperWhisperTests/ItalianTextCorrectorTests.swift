import XCTest

@testable import OpenSuperWhisper

final class ItalianTextCorrectorTests: XCTestCase {

    // MARK: - The point of "conservative": ambiguous words must survive untouched

    /// Both spellings are valid Italian words, so a context-free rule would
    /// silently change meaning. These must never be rewritten here.
    func testAmbiguousWordsAreNeverTouched() {
        let untouched = [
            "e",        // "e" (and) vs "è" (is)
            "si",       // "si" (oneself) vs "sì" (yes)
            "la",       // "la" (the) vs "là" (there)
            "da",       // "da" (from) vs "dà" (gives)
            "ne",       // "ne" (of it) vs "né" (nor)
            "se",       // "se" (if) vs "sé" (self)
            "pero",     // "pero" (pear tree) vs "però" (but)
            "meta",     // "meta" (goal) vs "metà" (half)
            "giacche",  // "giacche" (jackets) vs "giacché" (since)
            "subito",   // "subito" (immediately) vs "subìto" (undergone)
            "te",       // "te" (you, pronoun) vs "tè" (tea)
            "eta",      // "eta" (Greek letter) vs "età" (age)
        ]

        for word in untouched {
            let sentence = "questo \(word) qui"
            XCTAssertEqual(
                ItalianTextCorrector.correct(sentence), sentence,
                "'\(word)' is ambiguous and must be left alone"
            )
        }
    }

    func testAmbiguousWordsSurviveInRealisticSentences() {
        // "e" must not become "è" even where a human would write "è".
        let cases = [
            "il pero e in giardino",
            "la meta del lavoro e finita",
            "si e fatto tardi",
        ]
        for sentence in cases {
            XCTAssertEqual(ItalianTextCorrector.correct(sentence), sentence)
        }
    }

    // MARK: - Accent restoration (unaccented form is not a word)

    func testRestoresUnambiguousAccents() {
        XCTAssertEqual(ItalianTextCorrector.correct("non so perche"), "non so perché")
        XCTAssertEqual(ItalianTextCorrector.correct("un po piu tardi"), "un po più tardi")
        XCTAssertEqual(ItalianTextCorrector.correct("si puo fare"), "si può fare")
        XCTAssertEqual(ItalianTextCorrector.correct("e gia pronto"), "e già pronto")
        XCTAssertEqual(ItalianTextCorrector.correct("vivo in citta"), "vivo in città")
        XCTAssertEqual(ItalianTextCorrector.correct("ci vediamo lunedi"), "ci vediamo lunedì")
        XCTAssertEqual(ItalianTextCorrector.correct("cioe non lo so"), "cioè non lo so")
    }

    func testAccentRestorationPreservesCapitalisation() {
        XCTAssertEqual(ItalianTextCorrector.correct("Perche no?"), "Perché no?")
        XCTAssertEqual(ItalianTextCorrector.correct("Citta di Roma"), "Città di Roma")
    }

    func testAlreadyAccentedTextIsUnchanged() {
        // What a good model already produces must survive verbatim.
        let good = "Effettivamente te la scrive, ora è proprio una roba un pochino più complicata."
        XCTAssertEqual(ItalianTextCorrector.correct(good), good)
    }

    // MARK: - Spellings that are never correct

    func testFixesAlwaysWrongSpellings() {
        XCTAssertEqual(ItalianTextCorrector.correct("aspetta un pò"), "aspetta un po'")
        XCTAssertEqual(ItalianTextCorrector.correct("qual'è il problema"), "qual è il problema")
        XCTAssertEqual(ItalianTextCorrector.correct("è un'altro giorno"), "è un altro giorno")
        XCTAssertEqual(ItalianTextCorrector.correct("siamo daccordo"), "siamo d'accordo")
    }

    /// The feminine elision is correct and must not be "fixed".
    func testFeminineElisionIsPreserved() {
        for correct in ["un'altra volta", "un'ora fa", "un'amica mia"] {
            XCTAssertEqual(ItalianTextCorrector.correct(correct), correct)
        }
    }

    // MARK: - Whitespace and punctuation

    func testRemovesSpaceBeforePunctuationAndCollapsesRuns() {
        XCTAssertEqual(ItalianTextCorrector.correct("ciao , come stai ?"), "ciao, come stai?")
        XCTAssertEqual(ItalianTextCorrector.correct("due  spazi"), "due spazi")
    }

    /// Spacing *after* punctuation is left alone on purpose: touching it would
    /// corrupt decimals, times and URLs.
    func testDoesNotInsertSpaceAfterPunctuation() {
        for text in ["costa 1,5 euro", "alle 10.30", "vai su example.com/pagina"] {
            XCTAssertEqual(ItalianTextCorrector.correct(text), text)
        }
    }

    // MARK: - Anglicisms

    /// English loanwords are normal in spoken Italian and must be transcribed
    /// as spoken — never translated or "corrected".
    func testEnglishLoanwordsAreUntouched() {
        let sentence = "sposta la call dopo il meeting, la deadline è il budget review"
        XCTAssertEqual(ItalianTextCorrector.correct(sentence), sentence)
    }

    // MARK: - Safety

    func testEmptyAndPlainTextAreSafe() {
        XCTAssertEqual(ItalianTextCorrector.correct(""), "")
        XCTAssertEqual(ItalianTextCorrector.correct("testo normale senza errori"),
                       "testo normale senza errori")
    }

    func testIsIdempotent() {
        let input = "perche un pò , qual'è  la citta ?"
        let once = ItalianTextCorrector.correct(input)
        XCTAssertEqual(ItalianTextCorrector.correct(once), once,
                       "Running the pass twice must not keep changing the text")
    }
}
