import XCTest

@testable import OpenSuperWhisper

/// Covers the part of reformulation that can be tested without loading a
/// multi-gigabyte model: the guard between the model's output and the user's
/// text. Every case here is about *not losing the dictation*.
@MainActor
final class ReformulationServiceTests: XCTestCase {

    private let original = "domani alle 10 non ci sarò, ah no, alle 10.30"

    // MARK: - Falling back to the raw transcription

    func testEmptyResponseFallsBackToTheOriginal() {
        XCTAssertEqual(ReformulationService.sanitize("", fallingBackTo: original), original)
        XCTAssertEqual(ReformulationService.sanitize("   \n ", fallingBackTo: original), original)
    }

    /// A rewrite is roughly as long as its input. Something far longer means the
    /// model answered the dictation instead of cleaning it, which would replace
    /// the user's text with an unrelated reply.
    func testAnsweringInsteadOfRewritingFallsBackToTheOriginal() {
        let answer = String(repeating: "Certo, ecco una risposta molto lunga. ", count: 10)
        XCTAssertGreaterThan(answer.count, original.count * 3)
        XCTAssertEqual(ReformulationService.sanitize(answer, fallingBackTo: original), original)
    }

    func testAModeratelyLongerRewriteIsAccepted() {
        let rewrite = "Domani non ci sarò alle 10.30."
        XCTAssertEqual(ReformulationService.sanitize(rewrite, fallingBackTo: original), rewrite)
    }

    // MARK: - Stripping model scaffolding

    func testStripsPreamble() {
        XCTAssertEqual(
            ReformulationService.sanitize("Testo riscritto: Domani non ci sarò.",
                                          fallingBackTo: original),
            "Domani non ci sarò."
        )
        XCTAssertEqual(
            ReformulationService.sanitize("Output: Domani non ci sarò.", fallingBackTo: original),
            "Domani non ci sarò."
        )
    }

    func testStripsWrappingQuotes() {
        XCTAssertEqual(
            ReformulationService.sanitize("\"Domani non ci sarò.\"", fallingBackTo: original),
            "Domani non ci sarò."
        )
    }

    /// Models reach for typographic quotes at least as often as ASCII ones, and
    /// an unstripped pair is pasted straight into the user's document.
    func testStripsTypographicQuotes() {
        for wrapped in ["\u{201C}Domani non ci sarò.\u{201D}",
                        "\u{00AB}Domani non ci sarò.\u{00BB}"] {
            XCTAssertEqual(
                ReformulationService.sanitize(wrapped, fallingBackTo: original),
                "Domani non ci sarò.",
                "quotes in \(wrapped) should have been stripped"
            )
        }
    }

    /// On a very short dictation, "3× the input" is a handful of characters, so
    /// a perfectly good rewrite would be discarded for being too long.
    func testShortDictationsAreNotRejectedForLength() {
        let short = "alle 10"
        let rewrite = "Ci vediamo domani mattina alle 10."
        XCTAssertGreaterThan(rewrite.count, short.count * 3,
                             "fixture is pointless unless the rewrite really exceeds 3×")
        XCTAssertEqual(ReformulationService.sanitize(rewrite, fallingBackTo: short), rewrite)
    }

    /// The allowance must not become a blank cheque: a genuine answer is still
    /// rejected, even when the input was short.
    func testTheShortAllowanceStillRejectsAnAnswer() {
        let short = "alle 10"
        let answer = String(repeating: "Certo, ecco una risposta lunghissima. ", count: 10)
        XCTAssertEqual(ReformulationService.sanitize(answer, fallingBackTo: short), short)
    }

    /// Quotes that are part of the sentence must survive — only a pair wrapping
    /// the *whole* response is scaffolding.
    func testQuotesInsideTheTextAreKept() {
        let quoted = "Mi ha detto \"arrivo\" e poi è sparito."
        XCTAssertEqual(ReformulationService.sanitize(quoted, fallingBackTo: original), quoted)
    }

    func testTrimsSurroundingWhitespace() {
        XCTAssertEqual(
            ReformulationService.sanitize("\n  Domani non ci sarò.  \n", fallingBackTo: original),
            "Domani non ci sarò."
        )
    }

    // MARK: - Anglicisms

    /// English loanwords are normal in spoken Italian; sanitisation must not be
    /// what mangles them.
    func testAnglicismsSurviveSanitisation() {
        let text = "Sposta la call dopo il meeting, la deadline è venerdì."
        XCTAssertEqual(ReformulationService.sanitize(text, fallingBackTo: original), text)
    }

    // MARK: - The prompt itself

    /// The two rules that are easy to lose in a prompt edit and expensive to
    /// notice: don't translate loanwords, don't answer.
    func testInstructionsForbidTranslatingAndAnswering() {
        XCTAssertTrue(ReformulationService.instructions.contains("NON tradurre"))
        XCTAssertTrue(ReformulationService.instructions.contains("NON rispondere"))
    }
}
