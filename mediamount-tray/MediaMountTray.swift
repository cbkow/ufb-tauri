import SwiftUI
import Foundation

/// Minimal MenuBarExtra app that shows mount status from the mediamount-agent.
/// Communicates with the Rust agent via Unix domain socket IPC.
@main
struct MediaMountTrayApp: App {
    @StateObject private var agent = AgentConnection()

    var body: some Scene {
        MenuBarExtra {
            VStack(alignment: .leading, spacing: 0) {
                Text("MediaMount")
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

                Button("Open Log") {
                    openLog()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 3)

                Divider()

                Button("Quit Agent") {
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
        // Try to find UFB app
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
        // Fallback: try to open by bundle ID
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

// MARK: - Mount State Model

struct MountInfo: Identifiable {
    let id: String
    let displayName: String
    var state: String
    var stateDetail: String
}

// MARK: - Agent IPC Connection

class AgentConnection: ObservableObject {
    @Published var mounts: [MountInfo] = []
    @Published var connected = false

    private let socketPath: String
    private var inputStream: InputStream?
    private var outputStream: OutputStream?
    private var buffer = Data()

    init() {
        // Same socket path as the Rust agent
        if let runtimeDir = ProcessInfo.processInfo.environment["XDG_RUNTIME_DIR"] {
            socketPath = "\(runtimeDir)/ufb/mediamount-agent.sock"
        } else {
            socketPath = "/tmp/ufb-mediamount-agent.sock"
        }
        connect()
    }

    func connect() {
        // Poll for connection in background
        DispatchQueue.global(qos: .background).async { [weak self] in
            self?.pollConnection()
        }
    }

    private func pollConnection() {
        while true {
            if tryConnect() {
                DispatchQueue.main.async { self.connected = true }
                readLoop()
                DispatchQueue.main.async {
                    self.connected = false
                    self.mounts = []
                }
            }
            Thread.sleep(forTimeInterval: 3.0)
        }
    }

    private func tryConnect() -> Bool {
        // Connect to Unix domain socket
        let socket = socket(AF_UNIX, SOCK_STREAM, 0)
        guard socket >= 0 else { return false }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = socketPath.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            close(socket)
            return false
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
            ptr.withMemoryRebound(to: CChar.self, capacity: pathBytes.count) { dest in
                for (i, byte) in pathBytes.enumerated() {
                    dest[i] = byte
                }
            }
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                Darwin.connect(socket, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }

        if connectResult < 0 {
            close(socket)
            return false
        }

        // Create streams from socket
        var readStream: Unmanaged<CFReadStream>?
        var writeStream: Unmanaged<CFWriteStream>?
        CFStreamCreatePairWithSocket(nil, socket, &readStream, &writeStream)

        guard let input = readStream?.takeRetainedValue() as InputStream?,
              let output = writeStream?.takeRetainedValue() as OutputStream? else {
            close(socket)
            return false
        }

        self.inputStream = input
        self.outputStream = output
        input.open()
        output.open()

        // Send GetStates command
        sendMessage(["type": "get_states"])

        return true
    }

    func readLoop() {
        guard let input = inputStream else { return }
        let bufferSize = 4096
        let readBuffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { readBuffer.deallocate() }

        while input.hasBytesAvailable || input.streamStatus == .open {
            if input.hasBytesAvailable {
                let bytesRead = input.read(readBuffer, maxLength: bufferSize)
                if bytesRead <= 0 { break }
                buffer.append(readBuffer, count: bytesRead)
                processBuffer()
            } else {
                Thread.sleep(forTimeInterval: 0.1)
            }
        }

        inputStream?.close()
        outputStream?.close()
        inputStream = nil
        outputStream = nil
    }

    private func processBuffer() {
        // Wire protocol: 4-byte little-endian length prefix + JSON
        while buffer.count >= 4 {
            let length = buffer.withUnsafeBytes { ptr -> UInt32 in
                ptr.load(as: UInt32.self).littleEndian
            }
            let totalLength = 4 + Int(length)
            guard buffer.count >= totalLength else { break }

            let jsonData = buffer.subdata(in: 4..<totalLength)
            buffer.removeSubrange(0..<totalLength)

            if let json = try? JSONSerialization.jsonObject(with: jsonData) as? [String: Any] {
                handleMessage(json)
            }
        }
    }

    private func handleMessage(_ msg: [String: Any]) {
        guard let type = msg["type"] as? String else { return }

        switch type {
        case "mount_state_update":
            guard let mountId = msg["mountId"] as? String,
                  let state = msg["state"] as? String,
                  let stateDetail = msg["stateDetail"] as? String else { return }

            DispatchQueue.main.async {
                if let idx = self.mounts.firstIndex(where: { $0.id == mountId }) {
                    self.mounts[idx].state = state
                    self.mounts[idx].stateDetail = stateDetail
                } else {
                    self.mounts.append(MountInfo(
                        id: mountId,
                        displayName: mountId,
                        state: state,
                        stateDetail: stateDetail
                    ))
                }
            }
        default:
            break
        }
    }

    func startMount(_ mountId: String) {
        sendMessage(["type": "start_mount", "mountId": mountId, "commandId": ""])
    }

    func stopMount(_ mountId: String) {
        sendMessage(["type": "stop_mount", "mountId": mountId, "commandId": ""])
    }

    func restartMount(_ mountId: String) {
        sendMessage(["type": "restart_mount", "mountId": mountId, "commandId": ""])
    }

    func sendQuit() {
        sendMessage(["type": "quit"])
    }

    /// Send quit command, then wait briefly for graceful shutdown.
    func quitAgent() {
        sendQuit()
        // Give the agent a moment to process the quit
        DispatchQueue.global().asyncAfter(deadline: .now() + 2.0) { [weak self] in
            // Check if agent is still running by trying the socket
            let sockPath = self?.socketPath ?? "/tmp/ufb-mediamount-agent.sock"
            if FileManager.default.fileExists(atPath: sockPath) {
                // Socket still exists — agent may not have exited
                self?.forceKillAgent()
            }
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    /// Force-kill the agent process if it didn't exit gracefully.
    func forceKillAgent() {
        // Find and kill mediamount-agent processes
        let task = Process()
        task.launchPath = "/usr/bin/pkill"
        task.arguments = ["-f", "mediamount-agent"]
        try? task.run()
        task.waitUntilExit()
        // Clean up stale socket
        try? FileManager.default.removeItem(atPath: socketPath)
    }

    private func sendMessage(_ dict: [String: Any]) {
        guard let output = outputStream,
              let jsonData = try? JSONSerialization.data(withJSONObject: dict) else { return }

        // Wire protocol: 4-byte LE length + JSON
        var length = UInt32(jsonData.count).littleEndian
        let lengthData = Data(bytes: &length, count: 4)

        lengthData.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: 4)
        }
        jsonData.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: jsonData.count)
        }
    }
}
