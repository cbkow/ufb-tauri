import Foundation

/// Manages the mediamount-agent lifecycle — starts on tray launch, stops on quit.
class AgentProcess {
    private var process: Process?

    /// Find the agent binary. Search order:
    /// 1. Bundled in the main UFB app: .app/Contents/Resources/mediamount-agent
    /// 2. Sibling to the tray app binary (dev builds)
    /// 3. Cargo debug build in the repo
    /// 4. System-wide install
    private func findAgentBinary() -> String? {
        let candidates = [
            // Bundled in the tray app's own Resources
            Bundle.main.resourcePath.map { $0 + "/mediamount-agent" },
            // Sibling in the parent app's Resources (when tray is bundled inside UFB.app)
            Bundle.main.bundlePath
                .replacingOccurrences(of: "/MediaMountTray.app", with: "")
                .replacingOccurrences(of: "/UFB.app", with: "")
                + "/mediamount-agent",
            // Parent app Resources
            Bundle.main.bundlePath + "/../../mediamount-agent",
            // Dev: cargo debug build (relative to repo)
            {
                // Walk up from the tray app to find the repo root
                var url = URL(fileURLWithPath: Bundle.main.bundlePath)
                for _ in 0..<10 {
                    url = url.deletingLastPathComponent()
                    let candidate = url.appendingPathComponent("mediamount-agent/target/debug/mediamount-agent")
                    if FileManager.default.isExecutableFile(atPath: candidate.path) {
                        return candidate.path
                    }
                }
                return nil
            }(),
            // Dev: hardcoded repo path (common dev location)
            "/Users/chris/Documents/GitHub/ufb-tauri/mediamount-agent/target/debug/mediamount-agent",
            // Homebrew or system path
            "/usr/local/bin/mediamount-agent",
        ]

        for candidate in candidates {
            guard let path = candidate else { continue }
            if FileManager.default.isExecutableFile(atPath: path) {
                return path
            }
        }
        return nil
    }

    /// Check if the agent is already running (e.g., started manually in terminal).
    private func isAgentRunning() -> Bool {
        let task = Process()
        task.launchPath = "/usr/bin/pgrep"
        task.arguments = ["-f", "mediamount-agent"]
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice
        try? task.run()
        task.waitUntilExit()
        return task.terminationStatus == 0
    }

    /// Start the agent if it's not already running.
    func start() {
        if isAgentRunning() {
            NSLog("[AgentProcess] Agent already running, skipping launch")
            return
        }

        guard let agentPath = findAgentBinary() else {
            NSLog("[AgentProcess] Could not find mediamount-agent binary")
            return
        }

        NSLog("[AgentProcess] Starting agent: \(agentPath)")

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: agentPath)
        proc.standardOutput = FileHandle.nullDevice
        proc.standardError = FileHandle.nullDevice

        do {
            try proc.run()
            self.process = proc
            NSLog("[AgentProcess] Agent started (pid \(proc.processIdentifier))")
        } catch {
            NSLog("[AgentProcess] Failed to start agent: \(error)")
        }
    }

    /// Stop the agent gracefully.
    func stop() {
        if let proc = process, proc.isRunning {
            NSLog("[AgentProcess] Terminating agent (pid \(proc.processIdentifier))")
            proc.terminate()
            // Give it a moment, then force kill if needed
            DispatchQueue.global().asyncAfter(deadline: .now() + 2.0) {
                if proc.isRunning {
                    NSLog("[AgentProcess] Force killing agent")
                    proc.interrupt()
                }
            }
        }
        process = nil
    }
}
