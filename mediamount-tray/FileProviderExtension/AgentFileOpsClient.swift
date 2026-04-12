import Foundation

/// IPC client for file operations with the mediamount-agent.
/// Connects to the agent's file operations socket in the app group container.
/// All calls are synchronous (blocking) — the FileProvider system calls extension
/// methods on its own work queues, so blocking is expected.
class AgentFileOpsClient {
    static let shared = AgentFileOpsClient()

    private let appGroupID = "5Z4S9VHV56.group.com.unionfiles.mediamount-tray"
    private var inputStream: InputStream?
    private var outputStream: OutputStream?
    private var buffer = Data()
    private let lock = NSLock()

    private var socketPath: String {
        let groupContainer = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        )
        if let container = groupContainer {
            return container.appendingPathComponent("fp.sock").path
        }
        // Fallback: try the hardcoded path
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home
            .appendingPathComponent("Library/Group Containers")
            .appendingPathComponent(appGroupID)
            .appendingPathComponent("fp.sock")
            .path
    }

    // MARK: - Public API

    /// List directory contents for a domain + relative path.
    func listDir(domain: String, relativePath: String) throws -> [DirEntryResponse] {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "list_dir",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]

        let response = try sendAndReceive(request)

        guard let type_ = response["type"] as? String else {
            throw FileOpsError.invalidResponse("Missing type field")
        }

        if type_ == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }

        guard type_ == "dir_listing",
              let entries = response["entries"] as? [[String: Any]] else {
            throw FileOpsError.invalidResponse("Expected dir_listing response")
        }

        return entries.compactMap { DirEntryResponse(json: $0) }
    }

    /// Get metadata for a single file/directory.
    func stat(domain: String, relativePath: String) throws -> FileStatResponse {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "stat",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]

        let response = try sendAndReceive(request)

        guard let type_ = response["type"] as? String else {
            throw FileOpsError.invalidResponse("Missing type field")
        }

        if type_ == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }

        guard type_ == "file_stat" else {
            throw FileOpsError.invalidResponse("Expected file_stat response")
        }

        guard let stat = FileStatResponse(json: response) else {
            throw FileOpsError.invalidResponse("Failed to parse file_stat")
        }
        return stat
    }

    /// Read a file — agent copies it to a temp path in the app group container.
    /// Returns the URL to the temp file.
    func readFile(domain: String, relativePath: String) throws -> (url: URL, stat: FileStatResponse) {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "read_file",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]

        let response = try sendAndReceive(request)

        guard let type_ = response["type"] as? String else {
            throw FileOpsError.invalidResponse("Missing type field")
        }

        if type_ == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }

        guard type_ == "file_ready",
              let tempPath = response["tempPath"] as? String else {
            throw FileOpsError.invalidResponse("Expected file_ready response")
        }

        let size = response["size"] as? UInt64 ?? 0
        let modified = response["modified"] as? Double ?? 0

        let stat = FileStatResponse(
            name: (relativePath as NSString).lastPathComponent,
            isDir: false,
            size: size,
            modified: modified,
            created: modified
        )

        return (URL(fileURLWithPath: tempPath), stat)
    }

    /// Staging directory in the app group container for file handoff to the agent.
    private var stagingDir: URL {
        let groupContainer = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Group Containers")
            .appendingPathComponent(appGroupID)
        return groupContainer.appendingPathComponent("staging")
    }

    /// Write a file to the NAS. For files, the extension stages the content in the app group
    /// container (where the agent can read it), then tells the agent to copy it to the NAS.
    /// For directory creation, pass isDir=true and sourceURL=nil.
    func writeFile(domain: String, relativePath: String, sourceURL: URL?, isDir: Bool = false) throws -> (size: UInt64, modified: Double) {
        var stagedPath = ""

        // Stage file content in the app group container so the agent can access it
        if let sourceURL = sourceURL, !isDir {
            let staging = stagingDir
            try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)

            let stagedFile = staging.appendingPathComponent(
                "\(ProcessInfo.processInfo.processIdentifier)-\(Int(Date().timeIntervalSince1970 * 1000)).tmp"
            )
            // Remove old staged file if exists
            try? FileManager.default.removeItem(at: stagedFile)

            try FileManager.default.copyItem(at: sourceURL, to: stagedFile)
            stagedPath = stagedFile.path
            NSLog("[FileOpsClient] Staged file: \(sourceURL.path) → \(stagedPath)")
        }

        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "write_file",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
            "sourcePath": stagedPath,
            "isDir": isDir,
        ]

        let response = try sendAndReceive(request)

        guard let type_ = response["type"] as? String else {
            throw FileOpsError.invalidResponse("Missing type field")
        }

        if type_ == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }

        guard type_ == "write_ok" else {
            throw FileOpsError.invalidResponse("Expected write_ok response")
        }

        let size = response["size"] as? UInt64 ?? 0
        let modified = response["modified"] as? Double ?? 0
        return (size, modified)
    }

    /// Delete a file or directory on the NAS.
    func deleteItem(domain: String, relativePath: String) throws {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "delete_item",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]

        let response = try sendAndReceive(request)

        guard let type_ = response["type"] as? String else {
            throw FileOpsError.invalidResponse("Missing type field")
        }

        if type_ == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }
    }

    // MARK: - Connection

    private func ensureConnected() throws {
        if inputStream != nil && outputStream != nil {
            return
        }

        let path = socketPath
        NSLog("[FileOpsClient] Connecting to %@", path)

        let socket = socket(AF_UNIX, SOCK_STREAM, 0)
        guard socket >= 0 else {
            throw FileOpsError.connectionFailed("Failed to create socket")
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = path.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            Darwin.close(socket)
            throw FileOpsError.connectionFailed("Socket path too long")
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
            Darwin.close(socket)
            throw FileOpsError.connectionFailed("connect() failed: \(String(cString: strerror(errno)))")
        }

        var readStream: Unmanaged<CFReadStream>?
        var writeStream: Unmanaged<CFWriteStream>?
        CFStreamCreatePairWithSocket(nil, socket, &readStream, &writeStream)

        guard let input = readStream?.takeRetainedValue() as InputStream?,
              let output = writeStream?.takeRetainedValue() as OutputStream? else {
            Darwin.close(socket)
            throw FileOpsError.connectionFailed("Failed to create streams")
        }

        input.open()
        output.open()
        self.inputStream = input
        self.outputStream = output
        self.buffer = Data()

        NSLog("[FileOpsClient] Connected")
    }

    private func disconnect() {
        inputStream?.close()
        outputStream?.close()
        inputStream = nil
        outputStream = nil
        buffer = Data()
    }

    // MARK: - Wire Protocol

    /// Send a request and wait for the response. Thread-safe via lock.
    private func sendAndReceive(_ request: [String: Any]) throws -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }

        // Try to connect (or reconnect)
        do {
            try ensureConnected()
        } catch {
            // Retry once after disconnect
            disconnect()
            try ensureConnected()
        }

        guard let output = outputStream, let input = inputStream else {
            throw FileOpsError.connectionFailed("Not connected")
        }

        // Serialize request
        let jsonData = try JSONSerialization.data(withJSONObject: request)

        // Write: 4-byte LE length + JSON
        var length = UInt32(jsonData.count).littleEndian
        let lengthData = Data(bytes: &length, count: 4)

        let writeResult1 = lengthData.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: 4)
        }
        guard writeResult1 == 4 else {
            disconnect()
            throw FileOpsError.connectionFailed("Failed to write length prefix")
        }

        let writeResult2 = jsonData.withUnsafeBytes { ptr in
            output.write(ptr.bindMemory(to: UInt8.self).baseAddress!, maxLength: jsonData.count)
        }
        guard writeResult2 == jsonData.count else {
            disconnect()
            throw FileOpsError.connectionFailed("Failed to write payload")
        }

        // Read response: 4-byte LE length + JSON
        let responseData = try readMessage(from: input)

        guard let json = try? JSONSerialization.jsonObject(with: responseData) as? [String: Any] else {
            throw FileOpsError.invalidResponse("Failed to parse JSON response")
        }

        return json
    }

    /// Read a length-prefixed message from the input stream.
    private func readMessage(from input: InputStream) throws -> Data {
        // Read 4-byte length prefix
        var lengthBuf = [UInt8](repeating: 0, count: 4)
        var bytesRead = 0
        while bytesRead < 4 {
            let n = input.read(&lengthBuf + bytesRead, maxLength: 4 - bytesRead)
            if n <= 0 {
                disconnect()
                throw FileOpsError.connectionFailed("Connection closed while reading length")
            }
            bytesRead += n
        }

        let length = Int(UInt32(littleEndian: lengthBuf.withUnsafeBytes { $0.load(as: UInt32.self) }))
        guard length > 0, length <= 16 * 1024 * 1024 else {
            throw FileOpsError.invalidResponse("Invalid message length: \(length)")
        }

        // Read payload
        var payload = Data(count: length)
        var totalRead = 0
        while totalRead < length {
            let n = payload.withUnsafeMutableBytes { ptr in
                input.read(ptr.bindMemory(to: UInt8.self).baseAddress! + totalRead, maxLength: length - totalRead)
            }
            if n <= 0 {
                disconnect()
                throw FileOpsError.connectionFailed("Connection closed while reading payload")
            }
            totalRead += n
        }

        return payload
    }

    private var requestCounter: UInt64 = 0
    private func makeRequestId() -> String {
        requestCounter += 1
        return "fp-\(requestCounter)"
    }
}

// MARK: - Response Models

struct DirEntryResponse {
    let name: String
    let isDir: Bool
    let size: UInt64
    let modified: Double
    let created: Double

    init?(json: [String: Any]) {
        guard let name = json["name"] as? String else { return nil }
        self.name = name
        self.isDir = json["isDir"] as? Bool ?? false
        self.size = json["size"] as? UInt64 ?? 0
        self.modified = json["modified"] as? Double ?? 0
        self.created = json["created"] as? Double ?? 0
    }
}

struct FileStatResponse {
    let name: String
    let isDir: Bool
    let size: UInt64
    let modified: Double
    let created: Double

    init?(json: [String: Any]) {
        guard let name = json["name"] as? String else { return nil }
        self.name = name
        self.isDir = json["isDir"] as? Bool ?? false
        self.size = json["size"] as? UInt64 ?? 0
        self.modified = json["modified"] as? Double ?? 0
        self.created = json["created"] as? Double ?? 0
    }

    init(name: String, isDir: Bool, size: UInt64, modified: Double, created: Double) {
        self.name = name
        self.isDir = isDir
        self.size = size
        self.modified = modified
        self.created = created
    }
}

// MARK: - Errors

enum FileOpsError: Error, LocalizedError {
    case connectionFailed(String)
    case invalidResponse(String)
    case agentError(String)

    var errorDescription: String? {
        switch self {
        case .connectionFailed(let msg): return "Agent connection failed: \(msg)"
        case .invalidResponse(let msg): return "Invalid response: \(msg)"
        case .agentError(let msg): return "Agent error: \(msg)"
        }
    }

    /// Convert to NSError in a domain that FileProvider accepts.
    var asNSError: NSError {
        switch self {
        case .connectionFailed:
            return NSError(domain: NSCocoaErrorDomain, code: NSFileReadNoSuchFileError,
                           userInfo: [NSLocalizedDescriptionKey: errorDescription ?? "Connection failed"])
        case .invalidResponse:
            return NSError(domain: NSCocoaErrorDomain, code: NSFileReadCorruptFileError,
                           userInfo: [NSLocalizedDescriptionKey: errorDescription ?? "Invalid response"])
        case .agentError(let msg):
            if msg.contains("not found") || msg.contains("No such file") || msg.contains("No enabled mount") {
                return NSError(domain: NSCocoaErrorDomain, code: NSFileReadNoSuchFileError,
                               userInfo: [NSLocalizedDescriptionKey: msg])
            }
            return NSError(domain: NSCocoaErrorDomain, code: NSFileReadUnknownError,
                           userInfo: [NSLocalizedDescriptionKey: msg])
        }
    }
}
