import Foundation
import Log

// A minimal HTTP/1.1 server over POSIX sockets — the Linux replacement for
// the iOS app's Network.framework listener. One accepted connection is
// handled to completion (Connection: close), bodies larger than the spill
// threshold stream to a temp file so a 3 GB audiobook never sits in RAM.

public struct LocalSendHTTPRequest: Sendable {
    public enum Body: Sendable {
        case memory(Data)
        case file(URL, byteCount: Int64)
    }

    public let method: String
    public let path: String
    /// Query items, percent-decoded.
    public let query: [String: String]
    public let headers: [String: String]
    /// Sender IP, IPv6-mapped prefixes stripped ("::ffff:a.b.c.d" → a.b.c.d).
    public let remoteHost: String?
    public let body: Body

    public func queryValue(_ name: String) -> String? {
        query[name]
    }

    var contentLength: Int64 {
        headers["content-length"].flatMap { Int64($0) } ?? 0
    }
}

public struct LocalSendHTTPResponse: Sendable {
    public let status: Int
    public let body: Data
    public let contentType: String

    public init(status: Int, body: Data = Data(), contentType: String = "application/json") {
        self.status = status
        self.body = body
        self.contentType = contentType
    }

    public static func json<T: Encodable>(_ value: T) -> LocalSendHTTPResponse {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(value) else { return .status(500) }
        return .init(status: 200, body: data)
    }

    public static func status(_ code: Int) -> LocalSendHTTPResponse { .init(status: code) }
    public static func badRequest() -> LocalSendHTTPResponse { status(400) }
    public static func forbidden() -> LocalSendHTTPResponse { status(403) }
    public static func notFound() -> LocalSendHTTPResponse { status(404) }
    public static func conflict() -> LocalSendHTTPResponse { status(409) }
    public static func unsupported() -> LocalSendHTTPResponse { status(422) }
}

/// Parses and serves one request per connection. Split out from the socket
/// plumbing so the protocol parser is unit-testable without a network.
public enum LocalSendHTTP {
    /// Bodies above this stream to a temp file instead of memory (iOS parity).
    public static let spillThreshold = 8 * 1_024 * 1_024

    static let methods: Set<String> = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS"]

    public enum HeadParseResult {
        case needMore
        case malformed
        case ok(head: Head, bodyOffset: Int)
    }

    public struct Head {
        public let method: String
        public let path: String
        public let query: [String: String]
        public let headers: [String: String]
    }

    /// Parses the head (request line + headers) from the buffer.
    public static func parseHead(_ data: Data) -> HeadParseResult {
        guard let range = data.range(of: Data("\r\n\r\n".utf8)) else { return .needMore }
        let head = String(decoding: data[data.startIndex..<range.lowerBound], as: UTF8.self)
        var lines = head.split(separator: "\r\n", omittingEmptySubsequences: false).makeIterator()

        guard let requestLine = lines.next() else { return .malformed }
        let parts = requestLine.split(separator: " ")
        guard parts.count == 3 else { return .malformed }
        let method = String(parts[0]).uppercased()
        guard methods.contains(method) else { return .malformed }

        let target = String(parts[1])
        var path = target
        var query: [String: String] = [:]
        if let questionMark = target.firstIndex(of: "?") {
            path = String(target[..<questionMark])
            let queryString = target[target.index(after: questionMark)...]
            for pair in queryString.split(separator: "&") {
                let kv = pair.split(separator: "=", maxSplits: 1).map(Self.percentDecode)
                if kv.count == 2 { query[kv[0]] = kv[1] }
                else if kv.count == 1 { query[kv[0]] = "" }
            }
        }

        var headers: [String: String] = [:]
        while let line = lines.next() {
            guard !line.isEmpty else { break }
            guard let colon = line.firstIndex(of: ":") else { return .malformed }
            let name = String(line[..<colon]).trimmingCharacters(in: .whitespaces).lowercased()
            let value = String(line[line.index(after: colon)...]).trimmingCharacters(in: .whitespaces)
            headers[name] = value
        }

        return .ok(head: Head(method: method, path: path, query: query, headers: headers),
                   bodyOffset: range.upperBound - data.startIndex)
    }

    public static func percentDecode(_ string: Substring) -> String {
        string.removingPercentEncoding ?? String(string)
    }

    static func reason(for status: Int) -> String {
        switch status {
        case 200: return "OK"
        case 400: return "Bad Request"
        case 403: return "Forbidden"
        case 404: return "Not Found"
        case 409: return "Conflict"
        case 422: return "Unprocessable Entity"
        case 500: return "Internal Server Error"
        default: return "Unknown"
        }
    }
}

/// The listener. `handler` runs on the receiver's actor — the server blocks
/// its per-connection thread until the (deliberately short) endpoint returns.
public final class LocalSendHTTPServer: @unchecked Sendable {
    private let spillThreshold = LocalSendHTTP.spillThreshold
    public var handler: (@Sendable (LocalSendHTTPRequest) -> LocalSendHTTPResponse)?

    private var listenFD: Int32 = -1
    private var acceptThread: Thread?
    private let stateLock = NSLock()
    private var running = false

    public init() {}

    /// Binds and starts accepting. Port candidates tried in order (53317,
    /// then an ephemeral port as fallback).
    public func start(portCandidates: [UInt16] = [53_317, 0], onReady: @escaping (UInt16) -> Void, onFailed: @escaping (String) -> Void) {
        stateLock.lock()
        if running {
            stateLock.unlock()
            return
        }
        stateLock.unlock()

        for candidate in portCandidates {
            guard let fd = bindPort(candidate) else { continue }
            let port = actualPort(fd)
            stateLock.lock()
            listenFD = fd
            running = true
            stateLock.unlock()
            let thread = Thread { [weak self] in
                self?.acceptLoop()
            }
            thread.name = "bookdrop-accept"
            thread.stackSize = 1 << 20
            acceptThread = thread
            thread.start()
            Log.info("BookDrop server listening on port \(port)")
            onReady(port)
            return
        }
        onFailed("could not bind any port")
    }

    public func stop() {
        stateLock.lock()
        let fd = listenFD
        listenFD = -1
        running = false
        stateLock.unlock()
        guard fd >= 0 else { return }
        shutdown(fd, Int32(SHUT_RDWR))
        close(fd)
    }

    public var isRunning: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return running
    }

    // MARK: - Sockets

    private func bindPort(_ port: UInt16) -> Int32? {
        let fd = socket(AF_INET, Int32(SOCK_STREAM.rawValue), 0)
        guard fd >= 0 else { return nil }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr = in_addr(s_addr: INADDR_ANY)
        let bindResult = withUnsafePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                bind(fd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bindResult == 0, listen(fd, 8) == 0 else {
            close(fd)
            return nil
        }
        return fd
    }

    private func actualPort(_ fd: Int32) -> UInt16 {
        var addr = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let result = withUnsafeMutablePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                getsockname(fd, sa, &len)
            }
        }
        guard result == 0 else { return 0 }
        return UInt16(bigEndian: addr.sin_port)
    }

    private func acceptLoop() {
        while true {
            stateLock.lock()
            let fd = listenFD
            let active = running
            stateLock.unlock()
            guard active, fd >= 0 else { return }

            var clientAddr = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            let client = withUnsafeMutablePointer(to: &clientAddr) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                    accept(fd, sa, &len)
                }
            }
            guard client >= 0 else { continue }

            let host = Self.hostString(clientAddr)
            let thread = Thread { [weak self] in
                self?.handleConnection(client, host: host)
            }
            thread.name = "bookdrop-conn"
            thread.stackSize = 1 << 20
            thread.start()
        }
    }

    private static func hostString(_ addr: sockaddr_in) -> String? {
        var addr = addr
        var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        inet_ntop(AF_INET, &addr.sin_addr, &buffer, socklen_t(INET_ADDRSTRLEN))
        let ip = buffer.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
        return ip == "0.0.0.0" ? nil : ip
    }

    private func handleConnection(_ fd: Int32, host: String?) {
        defer { close(fd) }

        var buffer = Data()
        let chunk = 64 * 1_024
        var chunkBytes = [UInt8](repeating: 0, count: chunk)

        // Read until the full head has arrived.
        while true {
            if buffer.count > 1_048_576 { return }  // head absurdly large
            switch LocalSendHTTP.parseHead(buffer) {
            case .needMore:
                break  // keep reading
            case .malformed:
                write(.badRequest(), to: fd)
                return
            case .ok(let head, let bodyOffset):
                guard let request = readBody(
                    head: head, bodyOffset: bodyOffset, buffer: buffer, fd: fd, host: host
                ) else { return }
                let response = handler?(request) ?? .status(500)
                write(response, to: fd)
                return
            }
            let n = recv(fd, &chunkBytes, chunk, 0)
            guard n > 0 else { return }
            buffer.append(contentsOf: chunkBytes[0..<n])
        }
    }

    /// Reads the declared body (memory or spilled file) and assembles the
    /// request. Returns nil on socket error.
    private func readBody(
        head: LocalSendHTTP.Head,
        bodyOffset: Int,
        buffer: Data,
        fd: Int32,
        host: String?
    ) -> LocalSendHTTPRequest? {
        let length = head.headers["content-length"].flatMap { Int64($0) } ?? 0
        var bodyData = buffer.dropFirst(bodyOffset)
        var spilledURL: URL?
        let chunk = 64 * 1_024
        var chunkBytes = [UInt8](repeating: 0, count: chunk)

        if length > Int64(spillThreshold) {
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("bookdrop-body-\(UUID().uuidString)")
            guard FileManager.default.createFile(atPath: url.path, contents: nil) else { return nil }
            guard let handle = try? FileHandle(forWritingTo: url) else { return nil }
            defer { try? handle.close() }
            do {
                try handle.write(contentsOf: bodyData)
                var remaining = length - Int64(bodyData.count)
                while remaining > 0 {
                    let n = recv(fd, &chunkBytes, chunk, 0)
                    guard n > 0 else {
                        try? FileManager.default.removeItem(at: url)
                        return nil
                    }
                    try handle.write(contentsOf: chunkBytes[0..<n])
                    remaining -= Int64(n)
                }
            } catch {
                try? FileManager.default.removeItem(at: url)
                return nil
            }
            spilledURL = url
        } else {
            let needed = Int(length) - bodyData.count
            if needed > 0 {
                var rest = Data(capacity: needed)
                var chunkBytes = [UInt8](repeating: 0, count: chunk)
                var remaining = needed
                while remaining > 0 {
                    let n = recv(fd, &chunkBytes, min(chunk, remaining), 0)
                    guard n > 0 else { return nil }
                    rest.append(contentsOf: chunkBytes[0..<n])
                    remaining -= n
                }
                bodyData.append(rest)
            }
        }

        let body: LocalSendHTTPRequest.Body
        if let url = spilledURL {
            body = .file(url, byteCount: length)
        } else if length > Int64(spillThreshold) {
            body = .memory(Data())  // unreachable; satisfies the switch
        } else {
            body = .memory(Data(bodyData.prefix(Int(length))))
        }
        return LocalSendHTTPRequest(
            method: head.method,
            path: head.path,
            query: head.query,
            headers: head.headers,
            remoteHost: host,
            body: body
        )
    }

    private func write(_ response: LocalSendHTTPResponse, to fd: Int32) {
        var head = "HTTP/1.1 \(response.status) \(LocalSendHTTP.reason(for: response.status))\r\n"
        head += "Content-Type: \(response.contentType)\r\n"
        head += "Content-Length: \(response.body.count)\r\n"
        head += "Connection: close\r\n\r\n"
        var out = Data(head.utf8)
        out.append(response.body)
        out.withUnsafeBytes { raw in
            var sent = 0
            while sent < raw.count {
                let n = send(fd, raw.baseAddress!.advanced(by: sent), raw.count - sent, 0)
                guard n > 0 else { return }
                sent += n
            }
        }
    }
}
