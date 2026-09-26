import COnnxRuntime
import Foundation
import Log

enum OrtError: Error {
    case failed(String)
}

/// Process-wide ONNX Runtime access: the C API table (version-stable
/// struct of function pointers) and status-checked calls.
enum OrtRuntime {
    nonisolated(unsafe) static let api: UnsafePointer<OrtApi> = {
        guard let base = OrtGetApiBase(),
              let getApi = base.pointee.GetApi,
              let api = getApi(UInt32(ORT_API_VERSION)) else {
            fatalError("ONNX Runtime C API unavailable")
        }
        return api
    }()

    static func check(_ status: UnsafeMutablePointer<OrtStatus>?) throws {
        guard let status else { return }
        defer { api.pointee.ReleaseStatus!(status) }
        guard let message = api.pointee.GetErrorMessage!(status) else {
            throw OrtError.failed("unknown ONNX Runtime error")
        }
        throw OrtError.failed(String(cString: message))
    }

    /// Creates and destroys a bare environment — proves the shared library
    /// loads and the ABI matches, without needing a model.
    static func smokeTest() throws {
        var env: UnsafeMutablePointer<OrtEnv>?
        try check(api.pointee.CreateEnv!(ORT_LOGGING_LEVEL_WARNING, "speechnotes-smoke", &env))
        api.pointee.ReleaseEnv!(env)
    }
}

/// One file-backed ONNX session for a TTS voice model. Confined to the
/// engine worker (engines are handed to one run at a time), so the
/// unchecked Sendable is a confinement promise.
final class OrtModel: @unchecked Sendable {
    /// piper's stable input order; `sid` joins only on multi-speaker models.
    private var inputNames: [String]
    private let env: UnsafeMutablePointer<OrtEnv>
    private let session: UnsafeMutablePointer<OrtSession>
    private let memInfo: UnsafeMutablePointer<OrtMemoryInfo>
    /// NUL-terminated input/output names kept alive for the session's life.
    /// C's `const char* const*` imports as an array of OPTIONAL pointers.
    private var nameStorage: [UnsafeMutableBufferPointer<CChar>] = []
    private var inputNamePointers: [UnsafePointer<CChar>?] = []
    private var outputNamePointers: [UnsafePointer<CChar>?] = []

    init(modelPath: String, withSpeakerId: Bool = false) throws {
        let api = OrtRuntime.api
        var envOut: UnsafeMutablePointer<OrtEnv>?
        try OrtRuntime.check(api.pointee.CreateEnv!(ORT_LOGGING_LEVEL_WARNING, "speechnotes-tts", &envOut))
        let envLocal = envOut!

        var options: UnsafeMutablePointer<OrtSessionOptions>?
        try OrtRuntime.check(api.pointee.CreateSessionOptions!(&options))
        try OrtRuntime.check(api.pointee.SetIntraOpNumThreads!(options!, 2))

        var sessionOut: UnsafeMutablePointer<OrtSession>?
        try modelPath.withCString { path in
            try OrtRuntime.check(api.pointee.CreateSession!(envLocal, path, options!, &sessionOut))
        }
        let sessionLocal = sessionOut!

        var memOut: UnsafeMutablePointer<OrtMemoryInfo>?
        try OrtRuntime.check(api.pointee.CreateCpuMemoryInfo!(OrtDeviceAllocator, OrtMemTypeDefault, &memOut))
        let memLocal = memOut!

        var names = ["input", "input_lengths", "scales"]
        if withSpeakerId { names.append("sid") }
        var storage: [UnsafeMutableBufferPointer<CChar>] = []
        for name in names { storage.append(Self.cStringBuffer(name)) }
        storage.append(Self.cStringBuffer("output"))
        let inputPtrs: [UnsafePointer<CChar>?] =
            storage.prefix(names.count).map { UnsafePointer($0.baseAddress!) }
        let outputPtrs: [UnsafePointer<CChar>?] = [UnsafePointer(storage.last!.baseAddress!)]

        self.inputNames = names
        self.nameStorage = storage
        self.inputNamePointers = inputPtrs
        self.outputNamePointers = outputPtrs
        self.env = envLocal
        self.session = sessionLocal
        self.memInfo = memLocal
    }

    deinit {
        let api = OrtRuntime.api
        api.pointee.ReleaseSession!(session)
        api.pointee.ReleaseMemoryInfo!(memInfo)
        api.pointee.ReleaseEnv!(env)
        nameStorage.forEach { $0.deallocate() }
    }

    /// Runs the graph: ids + lengths + scales (+ sid) → float waveform.
    func synthesize(inputIds: [Int64], scales: [Float], speakerId: Int64) throws -> [Float] {
        let api = OrtRuntime.api
        var ids = inputIds
        var lengths = [Int64(inputIds.count)]
        var scaleBuffer = scales
        var sid = [speakerId]

        var values: [UnsafeMutablePointer<OrtValue>?] = [
            try int64Tensor(&ids, shape: [1, Int64(ids.count)]),
            try int64Tensor(&lengths, shape: [1]),
            try floatTensor(&scaleBuffer, shape: [Int64(scaleBuffer.count)]),
        ]
        if inputNames.contains("sid") {
            values.append(try int64Tensor(&sid, shape: [1]))
        }
        defer {
            for value in values where value != nil {
                api.pointee.ReleaseValue!(value)
            }
        }

        let inputValues: [UnsafePointer<OrtValue>?] = values.map { UnsafePointer($0) }
        var outputs = [UnsafeMutablePointer<OrtValue>?](repeating: nil, count: 1)
        try OrtRuntime.check(api.pointee.Run!(
            session, nil,
            inputNamePointers, inputValues, inputValues.count,
            outputNamePointers, 1,
            &outputs
        ))
        guard let out = outputs[0] else {
            throw OrtError.failed("model produced no output value")
        }

        var raw: UnsafeMutableRawPointer?
        try OrtRuntime.check(api.pointee.GetTensorMutableData!(out, &raw))
        var info: UnsafeMutablePointer<OrtTensorTypeAndShapeInfo>?
        try OrtRuntime.check(api.pointee.GetTensorTypeAndShape!(out, &info))
        var count: Int = 0
        try OrtRuntime.check(api.pointee.GetTensorShapeElementCount!(info!, &count))
        api.pointee.ReleaseTensorTypeAndShapeInfo!(info)

        guard count > 0, let raw else { return [] }
        return Array(UnsafeBufferPointer(start: raw.assumingMemoryBound(to: Float.self), count: count))
    }

    // MARK: - Tensors

    private func int64Tensor(
        _ values: inout [Int64], shape: [Int64]
    ) throws -> UnsafeMutablePointer<OrtValue> {
        var out: UnsafeMutablePointer<OrtValue>?
        try OrtRuntime.check(OrtRuntime.api.pointee.CreateTensorWithDataAsOrtValue!(
            memInfo, &values, MemoryLayout<Int64>.stride * values.count,
            shape, shape.count,
            ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
            &out
        ))
        return out!
    }

    private func floatTensor(
        _ values: inout [Float], shape: [Int64]
    ) throws -> UnsafeMutablePointer<OrtValue> {
        var out: UnsafeMutablePointer<OrtValue>?
        try OrtRuntime.check(OrtRuntime.api.pointee.CreateTensorWithDataAsOrtValue!(
            memInfo, &values, MemoryLayout<Float>.stride * values.count,
            shape, shape.count,
            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            &out
        ))
        return out!
    }

    private static func cStringBuffer(_ string: String) -> UnsafeMutableBufferPointer<CChar> {
        let nulTerminatedCount = string.utf8CString.count
        let buffer = UnsafeMutableBufferPointer<CChar>.allocate(capacity: nulTerminatedCount)
        string.withCString { src in
            for (i, byte) in UnsafeBufferPointer(start: src, count: nulTerminatedCount).enumerated() {
                buffer[i] = byte
            }
        }
        return buffer
    }
}
