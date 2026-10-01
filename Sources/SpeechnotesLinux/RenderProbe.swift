import Foundation
import Log

/// Main-thread render telemetry for diagnosing UI latency. Lightweight:
/// body evaluations just bump a counter; slow ones log rate-limited. The
/// SPEECHNOTES_BENCH=1 environment variable turns the app's first launch
/// into a pane-switch benchmark (timed, logged, then the process exits) —
/// the same transitions a user clicks, on the same code paths.
enum RenderProbe {
    nonisolated(unsafe) static var benchRequested =
        ProcessInfo.processInfo.environment["SPEECHNOTES_BENCH"] != nil

    nonisolated(unsafe) static var evalCount = 0
    /// SPEECHNOTES_BENCH=2 logs every evaluation, gaps included.
    nonisolated(unsafe) static let verbose =
        ProcessInfo.processInfo.environment["SPEECHNOTES_BENCH"] == "2"
    private nonisolated(unsafe) static var lastSlowLog: DispatchTime?
    @MainActor private static var lastEval: DispatchTime?

    /// Called at the end of every AppShell body evaluation.
    @MainActor
    static func recordBodyEval(pane: String, durationMs: Double) {
        evalCount += 1
        let now = DispatchTime.now()
        // The gap to the previous evaluation: how long the framework spent
        // between handing us a state change and asking for the next body.
        // A click's true latency is this gap plus our own body time, and the
        // gap is the part nobody was measuring.
        let sinceLast = lastEval.map {
            Double(now.uptimeNanoseconds - $0.uptimeNanoseconds) / 1e6
        } ?? 0
        lastEval = now
        guard verbose || durationMs > 80 || (sinceLast > 80) else { return }
        if !verbose,
           let last = lastSlowLog,
           now.uptimeNanoseconds - last.uptimeNanoseconds < 2_000_000_000 {
            return
        }
        lastSlowLog = now
        Log.info(String(
            format: "RENDER: body #%d (%@) self %.1fms, since previous %.1fms",
            evalCount, pane, durationMs, sinceLast
        ))
    }

    /// Wait until the render pass triggered by a state change has settled:
    /// at least one new body evaluation happened, then none for 120 ms.
    /// Returns the wall time from the call to the settle point.
    @MainActor
    static func settledLatency(timeoutMs: Double = 6000) async -> Double {
        let start = DispatchTime.now()
        let baseline = evalCount
        var lastSeen = baseline
        var lastChange = start
        while true {
            let now = DispatchTime.now()
            let elapsed = Double(now.uptimeNanoseconds - start.uptimeNanoseconds) / 1e6
            if evalCount > baseline,
               evalCount == lastSeen,
               now.uptimeNanoseconds - lastChange.uptimeNanoseconds > 120_000_000 {
                return elapsed
            }
            if elapsed > timeoutMs {
                Log.info(String(format: "RENDER: settle timeout after %.0fms", elapsed))
                return elapsed
            }
            if evalCount != lastSeen {
                lastSeen = evalCount
                lastChange = now
            }
            try? await Task.sleep(nanoseconds: 15_000_000)
        }
    }
}
