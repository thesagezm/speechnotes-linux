import XCTest
import Foundation
@testable import BookDrop

/// Drives the real socket listener end to end over a minimal raw-socket
/// HTTP client. URLSession is deliberately avoided: it cannot express what a
/// hostile peer would send (unknown methods, mismatched lengths) and it
/// drags in FoundationNetworking, which this test target does not link.
///
/// This suite exists for one specific reason: the server's connection
/// threads are NOT main-actor isolated, while the endpoint layer assumes the
/// main actor. `MainActor.assumeIsolated` asserts its executor and aborts the
/// process with SIGILL from any other thread — a crash, not a test failure.
/// Only a real socket reaching the real handler can catch it.
///
/// Every request runs on a detached thread and is awaited: XCTest on Linux
/// drives test methods from the main thread, so a blocking read there would
/// deadlock against the receiver's `DispatchQueue.main.sync` hop — which is
/// exactly the path under test.
///
/// Note: no `@MainActor` async `setUp` override — on Linux the base method
/// is nonisolated, so overriding it isolated and calling `super` fails to
/// compile. Each test starts and tears down its own receiver.
final class BookDropProtocolTests: XCTestCase {
    /// Why a raw request failed before it ever reached an assertion.
    enum ClientError: Error, CustomStringConvertible {
        case socketFailed(Int32)
        case connectFailed(Int32)
        case writeFailed(Int32)
        case noResponseHead(String)
        case unparsableStatus(String)

        var description: String {
            switch self {
            case .socketFailed(let e): return "socket() failed: \(e)"
            case .connectFailed(let e): return "connect() failed: \(e)"
            case .writeFailed(let e): return "send() failed: \(e)"
            case .noResponseHead(let s): return "no response head in \(s)"
            case .unparsableStatus(let s): return "unparsable status line: \(s)"
            }
        }
    }

    /// One raw HTTP request/response exchange. Runs off the main thread; all
    /// payloads are plain Sendable values so no test-case state is captured.
    nonisolated static func exchange(
        port: UInt16,
        method: String,
        target: String,
        body: Data,
        contentType: String?
    ) async throws -> (status: Int, body: Data) {
        try await Task.detached(priority: .userInitiated) {
            let fd = socket(AF_INET, Int32(SOCK_STREAM.rawValue), 0)
            guard fd >= 0 else { throw ClientError.socketFailed(errno) }
            defer { close(fd) }

            var addr = sockaddr_in()
            addr.sin_family = sa_family_t(AF_INET)
            addr.sin_port = port.bigEndian
            addr.sin_addr.s_addr = inet_addr("127.0.0.1")
            let connected = withUnsafePointer(to: &addr) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                    connect(fd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            guard connected == 0 else { throw ClientError.connectFailed(errno) }

            var head = "\(method) \(target) HTTP/1.1\r\nHost: 127.0.0.1\r\n"
            head += "Content-Length: \(body.count)\r\nConnection: close\r\n"
            if let contentType { head += "Content-Type: \(contentType)\r\n" }
            head += "\r\n"

            var out = Data(head.utf8)
            out.append(body)
            let sendFailure: Int32 = out.withUnsafeBytes { raw in
                var written = 0
                while written < raw.count {
                    let n = SwiftGlibc.send(
                        fd, raw.baseAddress!.advanced(by: written), raw.count - written, 0
                    )
                    if n <= 0 { return errno }
                    written += n
                }
                return 0
            }
            guard sendFailure == 0 else { throw ClientError.writeFailed(sendFailure) }

            var response = Data()
            var buffer = [UInt8](repeating: 0, count: 32 * 1024)
            while true {
                let n = recv(fd, &buffer, buffer.count, 0)
                if n <= 0 { break }
                response.append(contentsOf: buffer[0..<n])
            }
            guard let headerEnd = response.range(of: Data("\r\n\r\n".utf8)) else {
                throw ClientError.noResponseHead(
                    String(decoding: response.prefix(200), as: UTF8.self)
                )
            }
            let statusLine = String(
                decoding: response[response.startIndex..<headerEnd.lowerBound], as: UTF8.self
            )
            let parts = statusLine.split(separator: " ")
            guard parts.count >= 2, let status = Int(parts[1]) else {
                throw ClientError.unparsableStatus(statusLine)
            }
            return (status, Data(response[headerEnd.upperBound...]))
        }.value
    }

    // MARK: - Fixture

    /// The receiver under test and the port it bound.
    @MainActor
    private final class Fixture {
        let receiver: LocalSendReceiver
        var port: UInt16 { receiver.port }
        init() {
            receiver = LocalSendReceiver()
            receiver.start(portCandidates: [0])
        }
        func shutDown() { receiver.stop() }
    }

    @MainActor
    private var fixture: Fixture?

    @MainActor
    private func startFixture() -> Fixture {
        let made = Fixture()
        fixture = made
        return made
    }

    @MainActor
    private func send(
        _ method: String,
        _ target: String,
        body: Data = Data(),
        contentType: String? = nil
    ) async throws -> (status: Int, body: Data) {
        let port = try XCTUnwrap(fixture?.port, "fixture not started")
        return try await Self.exchange(
            port: port, method: method, target: target, body: body, contentType: contentType
        )
    }

    @MainActor
    private func prepareBody(files: [String: LocalSendFileMeta]) throws -> Data {
        let payload = LocalSendPrepareUpload(
            info: LocalSendDevice(
                alias: "test-sender", version: "2.2", deviceModel: nil, deviceType: "desktop",
                fingerprint: UUID().uuidString, port: 53317, protocolField: "http", download: false
            ),
            pin: nil,
            files: files
        )
        return try JSONEncoder().encode(payload)
    }

    @MainActor
    private func meta(
        _ name: String, size: Int64, sha256: String? = nil, id: String = "file-1"
    ) -> LocalSendFileMeta {
        LocalSendFileMeta(
            id: id, fileName: name, size: size, fileType: nil,
            sha256: sha256, preview: nil, metadata: nil
        )
    }

    /// prepare-upload, asserting 200 and decoding the session response.
    @MainActor
    private func prepare(_ files: [String: LocalSendFileMeta]) async throws -> LocalSendPrepareResponse {
        let (status, data) = try await send(
            "POST", "/api/localsend/v2/prepare-upload",
            body: try prepareBody(files: files), contentType: "application/json"
        )
        XCTAssertEqual(status, 200, "prepare-upload should be admitted")
        return try JSONDecoder().decode(LocalSendPrepareResponse.self, from: data)
    }

    // MARK: - Discovery

    @MainActor
    func testInfoEndpointAnnouncesTheReceiver() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, data) = try await send("GET", "/api/localsend/v2/info")
        XCTAssertEqual(status, 200)
        let device = try JSONDecoder().decode(LocalSendDevice.self, from: data)
        XCTAssertEqual(device.version, "2.2")
        XCTAssertEqual(device.port, Int(made.port))
        XCTAssertEqual(device.deviceType, "desktop")
        XCTAssertEqual(device.protocolField, "http")
        XCTAssertFalse(device.fingerprint.isEmpty)
        XCTAssertFalse(device.alias.isEmpty)
    }

    @MainActor
    func testRegisterEndpointAnswersWithTheDevice() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, data) = try await send(
            "POST", "/api/localsend/v2/register", body: Data("{}".utf8)
        )
        XCTAssertEqual(status, 200)
        XCTAssertFalse(try JSONDecoder().decode(LocalSendDevice.self, from: data).fingerprint.isEmpty)
    }

    @MainActor
    func testUnknownEndpointIsNotFound() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, _) = try await send("GET", "/nope")
        XCTAssertEqual(status, 404)
    }

    /// A junk request line must be refused, not fed onward.
    @MainActor
    func testMalformedRequestLineIsBadRequest() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, _) = try await send("BREW", "/api/localsend/v2/info")
        XCTAssertEqual(status, 400)
    }

    // MARK: - Prepare

    @MainActor
    func testPrepareWithGarbageBodyIsBadRequest() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, _) = try await send(
            "POST", "/api/localsend/v2/prepare-upload", body: Data("not json".utf8)
        )
        XCTAssertEqual(status, 400)
    }

    @MainActor
    func testUnsupportedExtensionIsRefused() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let body = try prepareBody(files: ["f": meta("payload.sh", size: 4, id: "f")])
        let (status, _) = try await send("POST", "/api/localsend/v2/prepare-upload", body: body)
        XCTAssertEqual(status, 403)
    }

    @MainActor
    func testOversizedDeclarationIsRefused() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let body = try prepareBody(files: [
            "f": meta("huge.epub", size: LocalSendReceiver.maxFileBytes + 1, id: "f")
        ])
        let (status, _) = try await send("POST", "/api/localsend/v2/prepare-upload", body: body)
        XCTAssertEqual(status, 403)
    }

    @MainActor
    func testAutoAcceptOffRefusesEverything() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        made.receiver.autoAccept = false
        let body = try prepareBody(files: ["f": meta("e.epub", size: 1, id: "f")])
        let (status, _) = try await send("POST", "/api/localsend/v2/prepare-upload", body: body)
        XCTAssertEqual(status, 403)
    }

    /// Two sessions at once must not both be admitted — the second would
    /// otherwise overwrite the first session's tokens.
    @MainActor
    func testSecondConcurrentPrepareIsRefused() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let body = try prepareBody(files: ["f": meta("d.epub", size: 1, id: "f")])
        let (first, _) = try await send("POST", "/api/localsend/v2/prepare-upload", body: body)
        XCTAssertEqual(first, 200)
        let (second, _) = try await send("POST", "/api/localsend/v2/prepare-upload", body: body)
        XCTAssertEqual(second, 409)
    }

    // MARK: - Upload

    /// The whole happy path: prepare → upload → the router is handed the file.
    @MainActor
    func testFullTransferLandsAndRoutes() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let payload = Data("chapter one text".utf8)
        let prepared = try await prepare([
            "file-1": meta("dropped.epub", size: Int64(payload.count), sha256: SHA256.hexDigest(payload))
        ])
        let token = try XCTUnwrap(prepared.files["file-1"])

        let routed = expectation(description: "router called")
        var routedURL: URL?
        made.receiver.router = { url in
            routedURL = url
            routed.fulfill()
        }

        let (status, _) = try await send(
            "POST",
            "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=file-1&token=\(token)",
            body: payload
        )
        XCTAssertEqual(status, 200)
        await fulfillment(of: [routed], timeout: 10)
        XCTAssertEqual(routedURL?.lastPathComponent, "dropped.epub")
    }

    @MainActor
    func testUploadWithWrongTokenIsForbidden() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let prepared = try await prepare(["file-1": meta("a.epub", size: 1)])
        let (status, _) = try await send(
            "POST",
            "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=file-1&token=wrong",
            body: Data("x".utf8)
        )
        XCTAssertEqual(status, 403)
    }

    @MainActor
    func testUploadWithUnknownSessionIsBadRequest() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let (status, _) = try await send(
            "POST", "/api/localsend/v2/upload?sessionId=nope&fileId=file-1&token=nope", body: Data("x".utf8)
        )
        XCTAssertEqual(status, 400)
    }

    /// Bytes that don't match the announced digest must never import.
    @MainActor
    func testUploadWithCorruptDigestIsRejected() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let prepared = try await prepare([
            "file-1": meta("b.epub", size: 5, sha256: SHA256.hexDigest(Data("the real content".utf8)))
        ])
        let token = try XCTUnwrap(prepared.files["file-1"])

        var routed = false
        made.receiver.router = { _ in routed = true }

        let (status, _) = try await send(
            "POST",
            "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=file-1&token=\(token)",
            body: Data("wrong".utf8)
        )
        XCTAssertEqual(status, 422)
        XCTAssertFalse(routed, "a digest mismatch must not reach the import pipeline")
    }

    /// Re-uploading the same file id must be refused, not silently accepted.
    @MainActor
    func testDuplicateUploadOfSameFileIsConflict() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let payload = Data("y".utf8)
        let prepared = try await prepare([
            "f1": meta("one.epub", size: Int64(payload.count), id: "f1"),
            "f2": meta("two.epub", size: Int64(payload.count), id: "f2"),
        ])
        let token = try XCTUnwrap(prepared.files["f1"])
        let upload = "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=f1&token=\(token)"

        let (first, _) = try await send("POST", upload, body: payload)
        XCTAssertEqual(first, 200)
        let (second, _) = try await send("POST", upload, body: payload)
        XCTAssertEqual(second, 409)
    }

    // MARK: - Cancel

    @MainActor
    func testCancelDiscardsTheSession() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let prepared = try await prepare(["file-1": meta("c.epub", size: 1)])
        let token = try XCTUnwrap(prepared.files["file-1"])

        let (cancelStatus, _) = try await send(
            "POST", "/api/localsend/v2/cancel?sessionId=\(prepared.sessionId)"
        )
        XCTAssertEqual(cancelStatus, 200)
        // The token died with the session.
        let (uploadStatus, _) = try await send(
            "POST",
            "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=file-1&token=\(token)",
            body: Data("y".utf8)
        )
        XCTAssertEqual(uploadStatus, 400)
    }

    // MARK: - Filename safety

    /// The landed name must never escape the session directory.
    @MainActor
    func testTraversingFileNameIsConfined() async throws {
        let made = startFixture()
        defer { made.shutDown(); fixture = nil }
        let payload = Data("z".utf8)
        let prepared = try await prepare([
            "file-1": meta("../../../../tmp/escaped.epub", size: Int64(payload.count))
        ])
        let token = try XCTUnwrap(prepared.files["file-1"])

        let routed = expectation(description: "router called")
        var landed: URL?
        made.receiver.router = { url in
            landed = url
            routed.fulfill()
        }
        let (status, _) = try await send(
            "POST",
            "/api/localsend/v2/upload?sessionId=\(prepared.sessionId)&fileId=file-1&token=\(token)",
            body: payload
        )
        XCTAssertEqual(status, 200)
        await fulfillment(of: [routed], timeout: 10)
        XCTAssertEqual(landed?.lastPathComponent, "escaped.epub")
        XCTAssertFalse(
            (landed?.path ?? "").contains("/tmp/escaped.epub"),
            "a traversing name escaped the session dir: \(landed?.path ?? "?")"
        )
    }
}
