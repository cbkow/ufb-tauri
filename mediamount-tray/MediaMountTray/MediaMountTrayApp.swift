import SwiftUI

/// Minimal MenuBarExtra app that shows mount status from the mediamount-agent.
/// Communicates with the Rust agent via Unix domain socket IPC. Manages the
/// Finder sidebar entries via `SidebarManager`; the FinderSync extension
/// (separate target) paints hydration badges in Finder.
@main
struct MediaMountTrayApp: App {
    @StateObject private var agent = AgentConnection()
    @StateObject private var sidebarManager = SidebarManager()
    private let agentProcess = AgentProcess()

    init() {
        agentProcess.start()

        // One-shot: remove any FileProvider domains left over from a
        // pre-Slice-5 install so they stop shadowing our NFS mounts in
        // the Finder sidebar.
        LegacyDomainCleanup.runOnce()

        // SidebarManager observes agent.$mounts via Combine once attached.
        // @StateObject isn't available during `init` (it's only valid inside
        // `body`), so defer attachment to a short async hop after launch —
        // the StateObject is materialized by then.
        let agentRef = agent
        let sidebarRef = sidebarManager
        DispatchQueue.main.async {
            sidebarRef.attach(to: agentRef)
        }
    }

    var body: some Scene {
        MenuBarExtra {
            VStack(alignment: .leading, spacing: 0) {
                Text("UFB")
                    .font(.headline)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)

                Divider()

                if agent.mounts.isEmpty {
                    Text("No mounts configured")
                        .foregroundColor(.secondary)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 4)
                } else {
                    ForEach(agent.mounts) { mount in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(mount.state == "mounted" ? Color.green : Color.red)
                                    .frame(width: 8, height: 8)
                                Text(mount.displayName)
                                    .font(.system(size: 12))
                                Text("— \(mount.stateDetail)")
                                    .foregroundColor(.secondary)
                                    .font(.system(size: 11))
                            }
                            HStack(spacing: 6) {
                                if mount.state == "mounted" || mount.state == "mounting" || mount.state == "initializing" {
                                    Button("Disconnect") { agent.stopMount(mount.id) }
                                } else {
                                    Button("Connect") { agent.startMount(mount.id) }
                                }
                                Button("Restart") { agent.restartMount(mount.id) }
                            }
                            .font(.caption)
                            .padding(.leading, 14)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 4)
                    }
                }

                Divider()

                Button("Open UFB") {
                    openUFB()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 3)

                Button("Show Mounts in Finder") {
                    let home = FileManager.default.homeDirectoryForCurrentUser
                    NSWorkspace.shared.open(home.appendingPathComponent("ufb/mounts"))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 3)

                Button("Open Log") {
                    openLog()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 3)

                Divider()

                Button("Quit") {
                    agentProcess.stop()
                    agent.quitAgent()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 3)
            }
            .frame(minWidth: 220)
        } label: {
            Image(systemName: "folder.fill")
        }
    }

    private func openUFB() {
        let candidates = [
            "/Applications/UFB.app",
            "/Applications/Union File Browser.app",
        ]
        for path in candidates {
            if FileManager.default.fileExists(atPath: path) {
                NSWorkspace.shared.open(URL(fileURLWithPath: path))
                return
            }
        }
        NSWorkspace.shared.launchApplication("ufb-tauri")
    }

    private func openLog() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let logPath = home.appendingPathComponent(".local/share/ufb/mediamount-agent.log")
        if FileManager.default.fileExists(atPath: logPath.path) {
            NSWorkspace.shared.open(logPath)
        }
    }
}
