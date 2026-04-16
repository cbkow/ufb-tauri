import Foundation
import AppKit

/// IPC connection to the mediamount-agent via Unix domain socket.
/// Uses length-prefixed JSON wire protocol (4-byte LE length + JSON payload).
class AgentConnection: ObservableObject {
    @Published var mounts: [MountInfo] = []
    @Published var connected = false

    private let socketPath: String
    private var inputStream: InputStream?
    private var outputStream: OutputStream?
    private var buffer = Data()

    init() {
        socketPath = AgentConnection.defaultSocketPath()
        connect()
    }

    /// Shared socket-path resolution — mirrors `unix_server::socket_path()`
    /// in the Rust agent. Lives inside the app group container on macOS so
    /// sandboxed extensions (FinderSync) can reach it. Filename is short
    /// (`a.sock`) so the full path stays under macOS's 104-byte `sun_path`
    /// limit for reasonable user home directory lengths.
    static func defaultSocketPath() -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let groupDir =
            "\(home)/Library/Group Containers/5Z4S9VHV56.group.com.unionfiles.mediamount-tray"
        return "\(groupDir)/a.sock"
    }

    func connect() {
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

    func quitAgent() {
        sendQuit()
        DispatchQueue.global().asyncAfter(deadline: .now() + 2.0) { [weak self] in
            let sockPath = self?.socketPath ?? AgentConnection.defaultSocketPath()
            if FileManager.default.fileExists(atPath: sockPath) {
                self?.forceKillAgent()
            }
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    func forceKillAgent() {
        let task = Process()
        task.launchPath = "/usr/bin/pkill"
        task.arguments = ["-f", "mediamount-agent"]
        try? task.run()
        task.waitUntilExit()
        try? FileManager.default.removeItem(atPath: socketPath)
    }

    private func sendMessage(_ dict: [String: Any]) {
        guard let output = outputStream,
              let jsonData = try? JSONSerialization.data(withJSONObject: dict) else { return }

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
