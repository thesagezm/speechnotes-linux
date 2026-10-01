import Foundation

// Wire types for the LocalSend protocol v2.2 receiver — ported from the iOS
// app (speechnotes-ios App/Sources/Services/LocalSend). Field names ARE the
// wire format — the coding keys are deliberate; don't "tidy" them.

/// The peer/device announcement — both directions of discovery carry this.
public struct LocalSendDevice: Codable, Equatable, Sendable {
    public var alias: String
    public var version: String
    public var deviceModel: String?
    /// mobile | desktop | web | headless | server
    public var deviceType: String?
    public var fingerprint: String
    public var port: Int
    /// http | https — wire key is "protocol".
    public var protocolField: String
    public var download: Bool?

    enum CodingKeys: String, CodingKey {
        case alias, version, deviceModel, deviceType, fingerprint, port, download
        case protocolField = "protocol"
    }

    public init(
        alias: String, version: String, deviceModel: String?, deviceType: String?,
        fingerprint: String, port: Int, protocolField: String, download: Bool?
    ) {
        self.alias = alias
        self.version = version
        self.deviceModel = deviceModel
        self.deviceType = deviceType
        self.fingerprint = fingerprint
        self.port = port
        self.protocolField = protocolField
        self.download = download
    }
}

/// One file offered in a prepare-upload request.
public struct LocalSendFileMeta: Codable, Equatable, Sendable {
    public let id: String
    public let fileName: String
    public let size: Int64
    public let fileType: String?
    public let sha256: String?
    public let preview: String?
    public let metadata: FileMetadata?

    public struct FileMetadata: Codable, Equatable, Sendable {
        public let modified: String?
        public let accessed: String?
    }
}

/// POST /api/localsend/v2/prepare-upload request body.
public struct LocalSendPrepareUpload: Codable, Sendable {
    public let info: LocalSendDevice
    public let pin: String?
    public let files: [String: LocalSendFileMeta]
}

/// prepare-upload's 200 response — sessionId plus a per-file token the
/// sender must echo on each upload.
public struct LocalSendPrepareResponse: Codable, Sendable {
    public let sessionId: String
    public let files: [String: String]
}
