import Foundation
import Log
import SwiftCrossUI

/// One completed (or routed) transfer, for the BookDrop history list.
public struct BookDropRecord: Identifiable, Equatable, Sendable {
    public let id = UUID()
    public let name: String
    public let size: Int64
    public let outcome: Outcome
    public let at: Date

    public enum Outcome: Equatable, Sendable {
        case imported(String)
        case failed(String)
        case rejected
    }

    public init(name: String, size: Int64, outcome: Outcome, at: Date) {
        self.name = name
        self.size = size
        self.outcome = outcome
        self.at = at
    }
}

/// The BookDrop receiver: a LocalSend protocol v2.2 peer that ACCEPTS
/// incoming transfers (the user's LocalSend app on the PC and Readest's
/// Nearby BookDrop speak the same protocol). Ported from the iOS app.
///
/// Receive-only, same as iOS: LocalSend senders find peers by a /24 unicast
/// scan hitting the HTTP /register endpoint, so no multicast plumbing is
/// needed — only this server being up on the LAN.
///
/// Concurrency mirrors iOS: the HTTP server's connection threads block while
/// the (deliberately short) endpoint logic runs on the main actor; imports
/// continue async afterwards.
@MainActor
public final class LocalSendReceiver: ObservableObject {
    public static let shared = LocalSendReceiver()

    @Published public private(set) var isRunning = false
    @Published public private(set) var port: UInt16 = 0
    @Published public private(set) var lastError: String?
    @Published public private(set) var history: [BookDropRecord] = []

    /// Extensions we accept, route and import today.
    public static let acceptedExtensions: Set<String> = [
        "epub", "pdf", "m4b", "m4a", "mp4", "mp3", "jex",
    ]

    private let server = LocalSendHTTPServer()
    private var sessions: [String: Session] = [:]
    private var timeoutTasks: [String: Task<Void, Never>] = [:]

    /// Hard cap on one file's declared size. A hostile (or buggy) peer can
    /// claim an absurd content-length and the reader would happily write it;
    /// the books we accept top out well under this.
    static let maxFileBytes: Int64 = 2 << 30

    /// Set by the app at launch: routes a landed file into the import
    /// pipeline (books store / JEX importer). Receives a file in our own
    /// temp space. MainActor: it touches the stores.
    public var router: (@MainActor (URL) async -> Void)?

    /// Off (opt-in) by default: BookDrop binds a LAN-facing port.
    @Published public var autoAccept: Bool = true

    public init() {
        // The HTTP server hands every request straight to `handle`, which is
        // nonisolated and does its own main-actor hop.
        server.handler = { [weak self] request in
            guard let self else { return .status(500) }
            return self.handle(request)
        }
    }

    /// Tests construct their own receiver; the app uses the singleton.
    public static func makeIsolated() -> LocalSendReceiver {
        LocalSendReceiver()
    }

    public func setEnabled(_ enabled: Bool) {
        if enabled {
            start()
        } else {
            stop()
        }
    }

    // MARK: - Lifecycle

    public func start(portCandidates: [UInt16] = [53_317, 0]) {
        guard !isRunning else { return }
        lastError = nil
        server.start(portCandidates: portCandidates) { [weak self] port in
            self?.onMain {
                self?.isRunning = true
                self?.port = port
            }
        } onFailed: { [weak self] message in
            self?.onMain {
                self?.isRunning = false
                self?.lastError = message
            }
        }
    }

    /// Runs work on the main actor from the server's plain threads. The
    /// `assumeIsolated` inside is only sound once the dispatch hop is done.
    nonisolated private func onMain(_ work: @escaping @MainActor () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated(work)
        } else {
            DispatchQueue.main.sync { MainActor.assumeIsolated(work) }
        }
    }

    public func stop() {
        server.stop()
        isRunning = false
        port = 0
        for session in sessions.values { discardSession(session) }
        sessions.removeAll()
    }

    // MARK: - Identity

    private var deviceAlias: String {
        if let env = ProcessInfo.processInfo.environment["BOOKDROP_ALIAS"], !env.isEmpty {
            return env
        }
        return Host.current().localizedName ?? "Speechnotes Linux"
    }

    private var fingerprint: String {
        let key = "bookDropFingerprint"
        let defaults = UserDefaults.standard
        if let existing = defaults.string(forKey: key) {
            return existing
        }
        let fresh = UUID().uuidString + UUID().uuidString
        defaults.set(fresh, forKey: key)
        return fresh
    }

    private var deviceDTO: LocalSendDevice {
        LocalSendDevice(
            alias: deviceAlias,
            version: "2.2",
            deviceModel: nil,
            deviceType: "desktop",
            fingerprint: fingerprint,
            port: Int(port),
            protocolField: "http",
            download: false
        )
    }

    // MARK: - Request routing

    /// Entry from the server's connection thread: block that thread while the
    /// main actor runs the endpoint. The `DispatchQueue.main.sync` hop is
    /// load-bearing — `assumeIsolated` asserts a MainActor executor and traps
    /// with SIGILL from any other thread (verified: it crashes), so without
    /// this wrapper a single LAN request kills the app. Endpoints are
    /// deliberately short — the only heavyweight step is the optional sha256
    /// of a landed file, once per file during a user-initiated transfer.
    nonisolated private func handle(_ request: LocalSendHTTPRequest) -> LocalSendHTTPResponse {
        DispatchQueue.main.sync {
            MainActor.assumeIsolated {
                handleOnMain(request)
            }
        }
    }

    private func handleOnMain(_ request: LocalSendHTTPRequest) -> LocalSendHTTPResponse {
        switch (request.method, request.path) {
        case ("POST", "/api/localsend/v2/register"), ("GET", "/api/localsend/v2/info"):
            return .json(deviceDTO)
        case ("POST", "/api/localsend/v2/prepare-upload"):
            return respondPrepareUpload(request)
        case ("POST", "/api/localsend/v2/upload"):
            return respondUpload(request)
        case ("POST", "/api/localsend/v2/cancel"):
            return respondCancel(request)
        default:
            return .notFound()
        }
    }

    private func respondPrepareUpload(_ request: LocalSendHTTPRequest) -> LocalSendHTTPResponse {
        guard case .memory(let data) = request.body,
              let payload = try? JSONDecoder().decode(LocalSendPrepareUpload.self, from: data) else {
            return .badRequest()
        }
        guard autoAccept else { return .forbidden() }
        guard sessions.isEmpty else { return .conflict() }  // one transfer at a time

        let accepted = payload.files.filter {
            LocalSendReceiver.acceptedExtensions.contains(Self.fileExtension($0.value.fileName))
                && $0.value.size <= Self.maxFileBytes
        }
        // Partial accept: omitted file ids count as rejected per spec.
        guard !accepted.isEmpty else { return .forbidden() }

        let session = Session(senderHost: request.remoteHost, files: accepted)
        sessions[session.id] = session
        armTimeout(session)
        Log.info("BookDrop: prepared \(accepted.count) file(s) from \(request.remoteHost ?? "?")")
        return .json(LocalSendPrepareResponse(sessionId: session.id, files: session.tokens))
    }

    private func respondUpload(_ request: LocalSendHTTPRequest) -> LocalSendHTTPResponse {
        guard let sessionId = request.queryValue("sessionId"),
              let fileId = request.queryValue("fileId"),
              let token = request.queryValue("token"),
              let session = sessions[sessionId] else {
            return .badRequest()
        }
        // A third party must not inject files into someone's session.
        guard session.senderHost == nil || session.senderHost == request.remoteHost else {
            return .forbidden()
        }
        guard session.tokens[fileId] == token else {
            return .forbidden()
        }
        guard !session.receivedIds.contains(fileId) else {
            return .conflict()  // duplicate upload of the same file
        }
        guard let meta = session.files[fileId] else {
            return .badRequest()
        }
        switch request.body {
        case .file(let url, let byteCount):
            guard byteCount == meta.size else { return .unsupported() }
            if let expected = meta.sha256, !expected.isEmpty,
               SHA256.hexDigest((try? Data(contentsOf: url, options: .mappedIfSafe)) ?? Data())
                != expected.lowercased() {
                return .unsupported()  // 422: sha mismatch
            }
            return land(url: url, session: session, fileId: fileId, meta: meta)
        case .memory(let data):
            guard Int64(data.count) == meta.size else { return .unsupported() }
            if let expected = meta.sha256, !expected.isEmpty,
               SHA256.hexDigest(data) != expected.lowercased() {
                return .unsupported()
            }
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("bookdrop-body-\(UUID().uuidString)")
            guard (try? data.write(to: url, options: .atomic)) != nil else {
                return .status(500)
            }
            return land(url: url, session: session, fileId: fileId, meta: meta)
        }
    }

    /// Moves a verified body into the session dir (same volume → a rename)
    /// and routes when the last file lands.
    private func land(url: URL, session: Session, fileId: String, meta: LocalSendFileMeta) -> LocalSendHTTPResponse {
        let target = session.dir.appendingPathComponent(Self.safeFileName(meta.fileName, fallback: fileId))
        do {
            if FileManager.default.fileExists(atPath: target.path) {
                try FileManager.default.removeItem(at: target)
            }
            try FileManager.default.moveItem(at: url, to: target)
        } catch {
            return .status(500)
        }
        session.receivedIds.insert(fileId)
        if session.receivedIds.count >= session.files.count {
            complete(session)
        }
        return .status(200)
    }

    private func respondCancel(_ request: LocalSendHTTPRequest) -> LocalSendHTTPResponse {
        guard let sessionId = request.queryValue("sessionId"), let session = sessions[sessionId] else {
            return .badRequest()
        }
        discardSession(session)
        return .status(200)
    }

    // MARK: - Session plumbing

    private final class Session {
        let id = UUID().uuidString
        let senderHost: String?
        let dir: URL
        var files: [String: LocalSendFileMeta]
        var tokens: [String: String]
        var receivedIds: Set<String> = []

        init(senderHost: String?, files: [String: LocalSendFileMeta]) {
            self.senderHost = senderHost
            self.files = files
            self.dir = FileManager.default.temporaryDirectory
                .appendingPathComponent("BookDrop-\(UUID().uuidString)", isDirectory: true)
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            tokens = files.keys.reduce(into: [:]) { acc, key in
                acc[key] = UUID().uuidString
            }
        }
    }

    private func armTimeout(_ session: Session) {
        timeoutTasks[session.id]?.cancel()
        timeoutTasks[session.id] = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 5 * 60 * 1_000_000_000)
            guard !Task.isCancelled else { return }
            self?.timedOut(sessionId: session.id)
        }
    }

    private func timedOut(sessionId: String) {
        guard let session = sessions[sessionId] else { return }
        let received = session.receivedIds.count
        let offered = session.files.count
        discardSession(session)
        if received > 0 {
            insertHistory(
                BookDropRecord(
                    name: "\(received) of \(offered) file\(offered == 1 ? "" : "s")",
                    size: 0,
                    outcome: .failed("Transfer timed out"),
                    at: Date()
                )
            )
        }
    }

    private func discardSession(_ session: Session) {
        timeoutTasks[session.id]?.cancel()
        timeoutTasks.removeValue(forKey: session.id)
        try? FileManager.default.removeItem(at: session.dir)
        sessions.removeValue(forKey: session.id)
    }

    /// All files landed — route each into the import pipeline, then clean.
    private func complete(_ session: Session) {
        let landed = session.files.values
            .filter { session.receivedIds.contains($0.id) }
            .map { Self.safeFileName($0.fileName, fallback: $0.id) }
        let dir = session.dir
        timeoutTasks[session.id]?.cancel()
        timeoutTasks.removeValue(forKey: session.id)
        sessions.removeValue(forKey: session.id)

        Task { [weak self] in
            for name in landed {
                let url = dir.appendingPathComponent(name)
                await self?.router?(url)
            }
            // The router consumes the files it handles; sweep anything left
            // over (rejected leftovers, failed imports) after a grace beat.
            try? await Task.sleep(nanoseconds: 30 * 1_000_000_000)
            try? FileManager.default.removeItem(at: dir)
        }
    }

    /// Called by the router after a file reached its destination so history
    /// reflects the truth (imported / failed).
    public func reportImport(name: String, size: Int64, outcome: BookDropRecord.Outcome) {
        insertHistory(BookDropRecord(name: name, size: size, outcome: outcome, at: Date()))
    }

    private func insertHistory(_ record: BookDropRecord) {
        history.insert(record, at: 0)
        if history.count > 20 {
            history.removeLast(history.count - 20)
        }
    }

    // MARK: - Helpers

    static func fileExtension(_ name: String) -> String {
        (name as NSString).pathExtension.lowercased()
    }

    public nonisolated static func safeFileName(_ name: String, fallback: String) -> String {
        let base = (name as NSString).lastPathComponent
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "\0", with: "")
        if base.isEmpty || base == "." || base == ".." {
            return "file-\(fallback)"
        }
        return base
    }
}
