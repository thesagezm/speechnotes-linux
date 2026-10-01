import XCTest
@testable import BookDrop

/// BookDrop (LocalSend v2.2 receiver) core: SHA-256 against NIST vectors,
/// filename sanitization, and the HTTP head parser. The full protocol flow
/// rides on these.
final class BookDropTests: XCTestCase {
    func testSHA256NISTVectors() {
        // SHA256("") = e3b0c442...
        XCTAssertEqual(
            SHA256.hexDigest(Data()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        )
        // SHA256("abc") = ba7816bf...
        XCTAssertEqual(
            SHA256.hexDigest(Data("abc".utf8)),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
        // 448-bit message ("abcdbcde...") — exercises the padding boundary.
        XCTAssertEqual(
            SHA256.hexDigest(Data("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".utf8)),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        )
    }

    func testSafeFileName() {
        XCTAssertEqual(LocalSendReceiver.safeFileName("book.epub", fallback: "x"), "book.epub")
        XCTAssertEqual(LocalSendReceiver.safeFileName("/etc/passwd", fallback: "x"), "passwd")
        XCTAssertEqual(LocalSendReceiver.safeFileName("..", fallback: "id"), "file-id")
        XCTAssertEqual(LocalSendReceiver.safeFileName("", fallback: "id"), "file-id")
        XCTAssertEqual(LocalSendReceiver.safeFileName("a\0b.txt", fallback: "id"), "ab.txt")
    }

    func testParseHeadGetWithQuery() {
        let raw = Data("GET /api/localsend/v2/info?a=1&b=hello%20world HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n".utf8)
        guard case .ok(let head, let offset) = LocalSendHTTP.parseHead(raw) else {
            return XCTFail("expected ok")
        }
        XCTAssertEqual(head.method, "GET")
        XCTAssertEqual(head.path, "/api/localsend/v2/info")
        XCTAssertEqual(head.query["a"], "1")
        XCTAssertEqual(head.query["b"], "hello world")
        XCTAssertEqual(offset, raw.count)
    }

    func testParseHeadNeedsMoreBytes() {
        guard case .needMore = LocalSendHTTP.parseHead(Data("GET /x HTTP/1.1\r\nHost: y\r\n".utf8)) else {
            return XCTFail("expected needMore")
        }
    }

    func testParseHeadMalformed() {
        guard case .malformed = LocalSendHTTP.parseHead(Data("BREW /x HTTP/1.1\r\n\r\n".utf8)) else {
            return XCTFail("expected malformed for unknown method")
        }
    }

    /// The reason phrase has to match the status code or strict LocalSend
    /// senders can reject the response.
    func testReasonPhrases() {
        XCTAssertEqual(LocalSendHTTP.reason(for: 200), "OK")
        XCTAssertEqual(LocalSendHTTP.reason(for: 400), "Bad Request")
        XCTAssertEqual(LocalSendHTTP.reason(for: 403), "Forbidden")
        XCTAssertEqual(LocalSendHTTP.reason(for: 404), "Not Found")
        XCTAssertEqual(LocalSendHTTP.reason(for: 409), "Conflict")
        XCTAssertEqual(LocalSendHTTP.reason(for: 422), "Unprocessable Entity")
        XCTAssertEqual(LocalSendHTTP.reason(for: 500), "Internal Server Error")
    }

    func testParseHeadRejectsHeaderWithoutColon() {
        guard case .malformed = LocalSendHTTP.parseHead(
            Data("GET /x HTTP/1.1\r\nBrokenHeader\r\n\r\n".utf8)
        ) else {
            return XCTFail("expected malformed for a header with no colon")
        }
    }

    /// The body offset must be measured from the buffer's START index — Data
    /// slices keep non-zero start indices, so this is the kind of arithmetic
    /// that silently truncates the first upload byte.
    func testParseHeadBodyOffsetOnSlicedBuffer() {
        var raw = Data("XXGET /api/localsend/v2/upload HTTP/1.1\r\nX-A: 1\r\n\r\nBODY".utf8)
        let slice = raw.dropFirst(2)
        guard case .ok(_, let offset) = LocalSendHTTP.parseHead(Data(slice)) else {
            return XCTFail("expected ok")
        }
        XCTAssertEqual(offset, Data(slice).count - 4)
        raw.removeAll()
    }
}
