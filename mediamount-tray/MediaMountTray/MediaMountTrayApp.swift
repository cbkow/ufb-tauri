import SwiftUI

/// Minimal MenuBarExtra app that shows mount status from the mediamount-agent.
/// Communicates with the Rust agent via Unix domain socket IPC. The FinderSync
/// extension (separate target) paints hydration badges in Finder.
///
/// No Finder sidebar integration — `LSSharedFileList`'s write path is broken
/// on macOS 26+ (kLSSharedFileListItemLast resolves to a bad pointer and
/// `LSSharedFileListInsertItemURL` segfaults in objc_retain). FileProvider's
/// auto-registered sidebar entries are the only programmatic path, and we
/// rejected that framework in Slice 5. Users bookmark mount paths manually
/// (drag `/Volumes/<share>` into Favorites once).
@main
struct MediaMountTrayApp: App {
    @StateObject private var agent = AgentConnection()
    private let agentProcess = AgentProcess()

    init() {
        agentProcess.start()
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
