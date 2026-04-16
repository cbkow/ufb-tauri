import Foundation

/// Unix-socket subscriber that listens to the agent's broadcast channel
/// for `BadgeUpdate` messages and forwards them to the FinderSync
/// controller. Mirrors the tray's `AgentConnection` but read-mostly —
/// only sends commands for drain/stats actions triggered from the Finder
/// context menu.
///
/// Push model: agent broadcasts BadgeUpdate on every hydration /
/// eviction. We don't maintain a local cache; the controller calls
/// `setBadgeIdentifier(_:for:)` on each update and lets FinderSync own
/// the current visible state.
///
/// Cold-start caveat: if FinderSync launches after the agent has been
/// running, it won't see badges for already-hydrated files until those
/// files transition state. Acceptable for v1 — badges are a visual cue,
/// not correctness. A full-state request message is a future add if
/// users report visible gaps.
class BadgeClient {
    /// Called when the agent reports a hydration-state change. The URL
    /// is the absolute path under `~/ufb/mounts/`. A nil identifier
    /// means "drop any existing badge."
    var onBadgeChange: ((URL, String?) -> Void)?

    /// Controller provides the mount-root prefix so BadgeClient can map
    /// `(domain, relpath)` pairs back to absolute `file://` URLs.
    var resolvePathToURL: ((_ domain: String, _ relpath: String) -> URL?)?

    private let socketPath: String
    private var inputStream: InputStream?
    private var outputStream: OutputStream?
    private var buffer = Data()
    private let ioQueue = DispatchQueue(label: "com.unionfiles.ufb.FinderSync.badge")

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        socketPath =
            "\(home)/Library/Group Containers/5Z4S9VHV56.group.com.unionfiles.mediamount-tray/mediamount-agent.sock"
    }

    func connect() {
        ioQueue.async { [weak self] in
            self?.pollConnection()
        }
    }

    private func pollConnection() {
        while true {
            if tryConnect() {
                readLoop()
            }
            Thread.sleep(forTimeInterval: 3.0)
        }
    }

    private func tryConnect() -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = socketPath.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            close(fd)
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
                Darwin.connect(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if connectResult < 0 {
            close(fd)
            return false
        }

        var readStream: Unmanaged<CFReadStream>?
        var writeStream: Unmanaged<CFWriteStream>?
        CFStreamCreatePairWithSocket(nil, fd, &readStream, &writeStream)
        guard let input = readStream?.takeRetainedValue() as InputStream?,
            let output = writeStream?.takeRetainedValue() as OutputStream?
        else {
            close(fd)
            return false
        }
        inputStream = input
        outputStream = output
        input.open()
        output.open()

        // Bootstrap: ask the agent to resend current mount states. The
        // agent's broadcast channel will then forward new BadgeUpdates
        // as they occur.
        sendMessage(["type": "get_states"])
        return true
    }

    private func readLoop() {
        guard let input = inputStream else { return }
        let bufferSize = 4096
        let readBuffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { readBuffer.deallocate() }

        while input.hasBytesAvailable || input.streamStatus == .open {
            if input.hasBytesAvailable {
                let n = input.read(readBuffer, maxLength: bufferSize)
                if n <= 0 { break }
                buffer.append(readBuffer, count: n)
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
        guard type == "badge_update" else { return }

        guard let domain = msg["domain"] as? String,
            let relpath = msg["relpath"] as? String,
            let badgeRaw = msg["badge"] as? String,
            let resolver = resolvePathToURL,
            let url = resolver(domain, relpath)
        else { return }

        let identifier: String?
        switch badgeRaw {
        case "hydrated": identifier = "ufb.hydrated"
        case "partial": identifier = "ufb.partial"
        case "uncached": identifier = nil
        default: identifier = nil
        }
        onBadgeChange?(url, identifier)
    }

    // MARK: - Commands

    /// Send a ClearSyncCache for the given share (mount id OR share name
    /// — the agent accepts both for backward compat). Invoked from the
    /// "Drain Cache" context menu.
    func drainShareCache(mountId: String) {
        ioQueue.async { [weak self] in
            self?.sendMessage([
                "type": "clear_sync_cache",
                "mountId": mountId,
                "commandId": "",
            ])
        }
    }

    private func sendMessage(_ dict: [String: Any]) {
        guard let output = outputStream,
            let data = try? JSONSerialization.data(withJSONObject: dict)
        else { return }
        var length = UInt32(data.count).littleEndian
        let lengthData = Data(bytes: &length, count: 4)
        _ = lengthData.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: 4)
        }
        _ = data.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: data.count)
        }
    }
}
