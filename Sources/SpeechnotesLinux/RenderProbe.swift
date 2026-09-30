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
    private nonisolated(unsafe) static var lastSlowLog: DispatchTime?

    /// Called at the end of every AppShell body evaluation.
    @MainActor
    static func recordBodyEval(pane: String, durationMs: Double) {
        evalCount += 1
        guard durationMs > 80 else { return }
        let now = DispatchTime.now()
        if let last = lastSlowLog,
           now.uptimeNanoseconds - last.uptimeNanoseconds < 2_000_000_000 {
            return
        }
        lastSlowLog = now
        Log.info(String(format: "RENDER: body #%d (%@) took %.0fms", evalCount, pane, durationMs))
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
