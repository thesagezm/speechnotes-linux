import COnnxRuntime
import Foundation
import Log

/// C enum cases don't import as Swift members — the pipeline spells them out.
enum OrtElementType {
    static let float: ONNXTensorElementDataType = ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT
    static let int64: ONNXTensorElementDataType = ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64
}

enum OrtError: Error {
    case failed(String)
}

/// Process-wide ONNX Runtime access: the C API table (version-stable
/// struct of function pointers) and status-checked calls.
enum OrtRuntime {
    nonisolated(unsafe) static let api: UnsafePointer<OrtApi> = {
        guard let base = OrtGetApiBase(), let getApi = base.pointee.GetApi,
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

/// A generic tensor value over the C API: shape + element type + borrowed
/// caller memory, exactly what `CreateTensorWithDataAsOrtValue` wants
/// (the buffer must outlive the copy into the session's arena — the
/// pipeline holds the source arrays for the duration of the call).
final class OrtValueRef: @unchecked Sendable {
    private let value: UnsafeMutablePointer<OrtValue>

    init<T>(
        tensorData values: [T],
        shape: [Int64],
        elementType: ONNXTensorElementDataType
    ) throws {
        let byteCount = values.count * MemoryLayout<T>.stride
        var out: UnsafeMutablePointer<OrtValue>?
        var infoOut: UnsafeMutablePointer<OrtMemoryInfo>?
        try OrtRuntime.check(OrtRuntime.api.pointee.CreateCpuMemoryInfo!(OrtDeviceAllocator, OrtMemTypeDefault, &infoOut))
        guard let info = infoOut else { throw OrtError.failed("CreateCpuMemoryInfo returned nil") }
        try values.withUnsafeBufferPointer { buffer in
            try OrtRuntime.check(OrtRuntime.api.pointee.CreateTensorWithDataAsOrtValue!(
                info,
                UnsafeMutableRawPointer(mutating: buffer.baseAddress),
                byteCount,
                shape, shape.count,
                elementType,
                &out
            ))
        }
        OrtRuntime.api.pointee.ReleaseMemoryInfo!(info)
        value = out!
    }

    /// Adopts a session-produced value (the Run call handed ownership over).
    init(adoptedTensor value: UnsafeMutablePointer<OrtValue>) {
        self.value = value
    }

    /// Raw tensor bytes as a typed array (the pipeline copies them out
    /// immediately, so no lifetime is retained).
    func tensorData<T>() throws -> [T] {
        var raw: UnsafeMutableRawPointer?
        try OrtRuntime.check(OrtRuntime.api.pointee.GetTensorMutableData!(value, &raw))
        var info: UnsafeMutablePointer<OrtTensorTypeAndShapeInfo>?
        try OrtRuntime.check(OrtRuntime.api.pointee.GetTensorTypeAndShape!(value, &info))
        var count: Int = 0
        try OrtRuntime.check(OrtRuntime.api.pointee.GetTensorShapeElementCount!(info!, &count))
        OrtRuntime.api.pointee.ReleaseTensorTypeAndShapeInfo!(info)
        guard count > 0, let raw else { return [] }
        return Array(UnsafeBufferPointer(
            start: raw.assumingMemoryBound(to: T.self),
            count: count * MemoryLayout<T>.stride / MemoryLayout<T>.stride
        ))
    }

    /// The raw session input pointer (the C API wants `const OrtValue*`).
    var pointer: UnsafePointer<OrtValue> {
        UnsafePointer(value)
    }

    deinit {
        OrtRuntime.api.pointee.ReleaseValue!(value)
    }
}

/// One file-backed ONNX session. Confined to a single worker (an engine
/// owns its sessions for the lifetime of one playback run), so the
/// unchecked Sendable is a confinement promise.
final class OrtSessionRef: @unchecked Sendable {
    private var env: UnsafeMutablePointer<OrtEnv>?
    private let session: UnsafeMutablePointer<OrtSession>
    private let options: UnsafeMutablePointer<OrtSessionOptions>
    private let memInfo: UnsafeMutablePointer<OrtMemoryInfo>

    /// Output tensor names the graph actually exposes (models differ:
    /// "waveform"/"audio"/"wav_tts"/"denoised_latent"…).
    let outputNames: [String]

    init(modelPath: String, threadCount: Int = 2) throws {
        let api = OrtRuntime.api
        var envOut: UnsafeMutablePointer<OrtEnv>?
        try OrtRuntime.check(api.pointee.CreateEnv!(ORT_LOGGING_LEVEL_WARNING, "speechnotes-tts", &envOut))
        guard let envLocal = envOut else { throw OrtError.failed("CreateEnv returned nil") }

        var optionsOut: UnsafeMutablePointer<OrtSessionOptions>?
        try OrtRuntime.check(api.pointee.CreateSessionOptions!(&optionsOut))
        guard let optionsLocal = optionsOut else { throw OrtError.failed("CreateSessionOptions returned nil") }
        try OrtRuntime.check(api.pointee.SetIntraOpNumThreads!(optionsLocal, Int32(threadCount)))

        var sessionOut: UnsafeMutablePointer<OrtSession>?
        try modelPath.withCString { path in
            try OrtRuntime.check(api.pointee.CreateSession!(envLocal, path, optionsLocal, &sessionOut))
        }
        guard let sessionLocal = sessionOut else { throw OrtError.failed("CreateSession returned nil") }

        var memOut: UnsafeMutablePointer<OrtMemoryInfo>?
        try OrtRuntime.check(api.pointee.CreateCpuMemoryInfo!(OrtDeviceAllocator, OrtMemTypeDefault, &memOut))
        guard let memLocal = memOut else { throw OrtError.failed("CreateCpuMemoryInfo returned nil") }
        self.memInfo = memLocal
        self.options = optionsLocal
        self.env = envLocal
        self.session = sessionLocal

        // Ask the model for its own output names rather than assuming.
        var count: Int = 0
        try OrtRuntime.check(api.pointee.SessionGetOutputCount!(session, &count))
        var names: [String] = []
        for index in 0..<count {
            var allocated: UnsafeMutablePointer<CChar>?
            try OrtRuntime.check(api.pointee.SessionGetOutputName!(
                session, index, OrtAllocatorInstance.defaultAllocator, &allocated
            ))
            guard let name = allocated else { continue }
            names.append(String(cString: name))
            api.pointee.AllocatorFree!(
                OrtAllocatorInstance.defaultAllocator, UnsafeMutableRawPointer(name)
            )
        }
        outputNames = names
    }

    /// Runs the graph with string-keyed inputs. Output values are only
    /// alive while the session arena is — the caller's `tensorData()` runs
    /// before the next call, which is the only safe pattern with the C API.
    func run(inputs: [String: OrtValueRef], outputNames: [String]?) throws -> [String: OrtValueRef] {
        let api = OrtRuntime.api
        let inputNames = Array(inputs.keys)
        let nameBuffers: [UnsafeMutableBufferPointer<CChar>] = inputNames.map { Self.cStringBuffer($0) }
        defer { nameBuffers.forEach { $0.deallocate() } }
        let inputNamePointers: [UnsafePointer<CChar>?] = nameBuffers.map { UnsafePointer($0.baseAddress!) }
        let inputValuePointers: [UnsafePointer<OrtValue>?] = inputNames.compactMap { inputs[$0]?.pointer }

        let requested = outputNames ?? self.outputNames
        let outputBuffers: [UnsafeMutableBufferPointer<CChar>] = requested.map { Self.cStringBuffer($0) }
        defer { outputBuffers.forEach { $0.deallocate() } }
        let outputNamePointers: [UnsafePointer<CChar>?] = outputBuffers.map { UnsafePointer($0.baseAddress!) }

        var outputs = [UnsafeMutablePointer<OrtValue>?](repeating: nil, count: requested.count)
        try OrtRuntime.check(api.pointee.Run!(
            session, nil,
            inputNamePointers, inputValuePointers, inputValuePointers.count,
            outputNamePointers, requested.count,
            &outputs
        ))

        var result: [String: OrtValueRef] = [:]
        for (index, name) in requested.enumerated() {
            guard let value = outputs[index] else { continue }
            result[name] = OrtValueRef(adoptedTensor: value)
        }
        return result
    }

    static func cStringBuffer(_ string: String) -> UnsafeMutableBufferPointer<CChar> {
        let nulTerminatedCount = string.utf8CString.count
        let buffer = UnsafeMutableBufferPointer<CChar>.allocate(capacity: nulTerminatedCount)
        string.withCString { src in
            for (i, byte) in UnsafeBufferPointer(start: src, count: nulTerminatedCount).enumerated() {
                buffer[i] = byte
            }
        }
        return buffer
    }

    deinit {
        let api = OrtRuntime.api
        api.pointee.ReleaseSession!(session)
        api.pointee.ReleaseMemoryInfo!(memInfo)
        api.pointee.ReleaseSessionOptions!(options)
        if let env { api.pointee.ReleaseEnv!(env) }
    }
}

/// The default allocator lives for the process; the C API hands it out on
/// demand (it's the only allocator usable with name-allocated strings).
enum OrtAllocatorInstance {
    nonisolated(unsafe) static let defaultAllocator: UnsafeMutablePointer<OrtAllocator> = {
        var out: UnsafeMutablePointer<OrtAllocator>?
        _ = try? OrtRuntime.check(OrtRuntime.api.pointee.GetAllocatorWithDefaultOptions!(&out))
        guard let allocator = out else { fatalError("ORT default allocator unavailable") }
        return allocator
    }()
}
