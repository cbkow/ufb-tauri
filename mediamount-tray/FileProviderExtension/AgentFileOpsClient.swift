import Foundation

/// IPC client for file operations with the mediamount-agent.
///
/// Uses a pool of independent Unix-socket connections so concurrent
/// FileProvider delegate methods (fetchContents, item, enumerateItems, etc.)
/// don't serialize behind a single mutex. Each pooled connection uses its own
/// lock to guarantee FIFO request/response framing on that connection; the
/// pool itself uses a semaphore to gate overall concurrency.
class AgentFileOpsClient {
    static let shared = AgentFileOpsClient()

    private let appGroupID = "5Z4S9VHV56.group.com.unionfiles.mediamount-tray"
    private let poolSize = 4

    /// Array of connections owned by this client. `acquire` / `release` move
    /// them between an idle set and an in-use set.
    private var idleConnections: [AgentConnection] = []
    private let poolLock = NSLock()
    private let semaphore: DispatchSemaphore

    private var socketPath: String {
        let groupContainer = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        )
        if let container = groupContainer {
            return container.appendingPathComponent("fp.sock").path
        }
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home
            .appendingPathComponent("Library/Group Containers")
            .appendingPathComponent(appGroupID)
            .appendingPathComponent("fp.sock")
            .path
    }

    private init() {
        self.semaphore = DispatchSemaphore(value: poolSize)
        let path = socketPath
        self.idleConnections = (0..<poolSize).map { _ in AgentConnection(socketPath: path) }
    }

    // MARK: - Public API (unchanged surface)

    func listDir(domain: String, relativePath: String) throws -> [DirEntryResponse] {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "list_dir",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "dir_listing",
              let entries = response["entries"] as? [[String: Any]] else {
            throw FileOpsError.invalidResponse("Expected dir_listing response")
        }
        return entries.compactMap { DirEntryResponse(json: $0) }
    }

    func stat(domain: String, relativePath: String) throws -> FileStatResponse {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "stat",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "file_stat",
              let stat = FileStatResponse(json: response) else {
            throw FileOpsError.invalidResponse("Expected file_stat response")
        }
        return stat
    }

    func readFile(domain: String, relativePath: String) throws -> (url: URL, stat: FileStatResponse) {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "read_file",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "file_ready",
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

    private var stagingDir: URL {
        let groupContainer = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupID
        ) ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Group Containers")
            .appendingPathComponent(appGroupID)
        return groupContainer.appendingPathComponent("staging")
    }

    func writeFile(domain: String, relativePath: String, sourceURL: URL?, isDir: Bool = false) throws -> (size: UInt64, modified: Double) {
        var stagedPath = ""
        if let sourceURL = sourceURL, !isDir {
            let staging = stagingDir
            try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
            let stagedFile = staging.appendingPathComponent(
                "\(ProcessInfo.processInfo.processIdentifier)-\(Int(Date().timeIntervalSince1970 * 1000)).tmp"
            )
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
        try throwIfError(response)
        guard response["type"] as? String == "write_ok" else {
            throw FileOpsError.invalidResponse("Expected write_ok response")
        }
        let size = response["size"] as? UInt64 ?? 0
        let modified = response["modified"] as? Double ?? 0
        return (size, modified)
    }

    func deleteItem(domain: String, relativePath: String) throws {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "delete_item",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
    }

    func renameItem(domain: String, oldPath: String, newPath: String) throws -> (size: UInt64, modified: Double) {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "rename_item",
            "requestId": requestId,
            "domain": domain,
            "oldPath": oldPath,
            "newPath": newPath,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "rename_ok" else {
            throw FileOpsError.invalidResponse("Expected rename_ok response")
        }
        let size = response["size"] as? UInt64 ?? 0
        let modified = response["modified"] as? Double ?? 0
        return (size, modified)
    }

    func getChanges(domain: String, sinceAnchor: String) throws -> ChangesResponse {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "get_changes",
            "requestId": requestId,
            "domain": domain,
            "sinceAnchor": sinceAnchor,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "changes" else {
            throw FileOpsError.invalidResponse("Expected changes response")
        }
        let updated = (response["updated"] as? [[String: Any]] ?? []).compactMap { ChangedEntryResponse(json: $0) }
        let deleted = response["deleted"] as? [String] ?? []
        let newAnchor = response["newAnchor"] as? String ?? ""
        let evict = response["evict"] as? [String] ?? []
        return ChangesResponse(updated: updated, deleted: deleted, evict: evict, newAnchor: newAnchor)
    }

    func recordEnumeration(domain: String, relativePath: String, entries: [DirEntryResponse]) {
        let requestId = makeRequestId()
        let entriesJson: [[String: Any]] = entries.map { entry in
            [
                "name": entry.name,
                "isDir": entry.isDir,
                "size": entry.size,
                "modified": entry.modified,
                "created": entry.created,
            ] as [String: Any]
        }
        let request: [String: Any] = [
            "type": "record_enumeration",
            "requestId": requestId,
            "domain": domain,
            "relativePath": relativePath,
            "entries": entriesJson,
        ]
        do {
            let _ = try sendAndReceive(request)
        } catch {
            NSLog("[FileOpsClient] recordEnumeration failed: \(error)")
        }
    }

    /// Query the agent for all currently-hydrated relative paths in a domain.
    /// Cheap — it's a single indexed SQLite query, no NAS I/O.
    func evictAll(domain: String) throws -> [String] {
        let requestId = makeRequestId()
        let request: [String: Any] = [
            "type": "evict_all",
            "requestId": requestId,
            "domain": domain,
        ]
        let response = try sendAndReceive(request)
        try throwIfError(response)
        guard response["type"] as? String == "evict_list" else {
            throw FileOpsError.invalidResponse("Expected evict_list response")
        }
        return response["paths"] as? [String] ?? []
    }

    // MARK: - Pool management

    private func acquire() -> AgentConnection {
        semaphore.wait()
        poolLock.lock()
        // By construction (semaphore gates to poolSize), there is always at
        // least one idle connection when we get here.
        let conn = idleConnections.removeLast()
        poolLock.unlock()
        return conn
    }

    private func release(_ conn: AgentConnection) {
        poolLock.lock()
        idleConnections.append(conn)
        poolLock.unlock()
        semaphore.signal()
    }

    private func sendAndReceive(_ request: [String: Any]) throws -> [String: Any] {
        let conn = acquire()
        defer { release(conn) }
        return try conn.sendAndReceive(request)
    }

    private func throwIfError(_ response: [String: Any]) throws {
        if response["type"] as? String == "error" {
            let message = response["message"] as? String ?? "Unknown error"
            throw FileOpsError.agentError(message)
        }
    }

    // Request IDs are advisory for logs — the pool uses FIFO framing, not
    // multiplexing, so request IDs don't need to coordinate across connections.
    private var requestCounter: UInt64 = 0
    private let counterLock = NSLock()
    private func makeRequestId() -> String {
        counterLock.lock()
        defer { counterLock.unlock() }
        requestCounter += 1
        return "fp-\(requestCounter)"
    }
}

// MARK: - Pooled connection

/// A single Unix-socket connection to the agent. All send/receive traffic on
/// this connection is serialized by `lock` (required because the agent's
/// wire protocol is FIFO request/response per connection, no multiplexing).
/// On any I/O error the connection is torn down and reconnected on the next
/// call; one retry before we propagate the error upward.
private final class AgentConnection {
    let socketPath: String
    private var inputStream: InputStream?
    private var outputStream: OutputStream?
    private var buffer = Data()
    private let lock = NSLock()

    init(socketPath: String) {
        self.socketPath = socketPath
    }

    func sendAndReceive(_ request: [String: Any]) throws -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }

        // One retry after a teardown+reconnect.
        do {
            try ensureConnected()
            return try sendAndReceiveLocked(request)
        } catch {
            NSLog("[FileOpsClient] Connection error, retrying: \(error)")
            disconnect()
            try ensureConnected()
            return try sendAndReceiveLocked(request)
        }
    }

    private func sendAndReceiveLocked(_ request: [String: Any]) throws -> [String: Any] {
        guard let output = outputStream, let input = inputStream else {
            throw FileOpsError.connectionFailed("Not connected")
        }

        let jsonData = try JSONSerialization.data(withJSONObject: request)

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

        let responseData = try readMessage(from: input)
        guard let json = try? JSONSerialization.jsonObject(with: responseData) as? [String: Any] else {
            throw FileOpsError.invalidResponse("Failed to parse JSON response")
        }
        return json
    }

    private func ensureConnected() throws {
        if inputStream != nil && outputStream != nil {
            return
        }

        let path = socketPath

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

        NSLog("[FileOpsClient] Pool connection opened")
    }

    private func disconnect() {
        inputStream?.close()
        outputStream?.close()
        inputStream = nil
        outputStream = nil
        buffer = Data()
    }

    private func readMessage(from input: InputStream) throws -> Data {
        var lengthBuf = [UInt8](repeating: 0, count: 4)
        var bytesRead = 0
        while bytesRead < 4 {
            let n = lengthBuf.withUnsafeMutableBufferPointer { ptr -> Int in
                input.read(ptr.baseAddress! + bytesRead, maxLength: 4 - bytesRead)
            }
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

struct ChangesResponse {
    let updated: [ChangedEntryResponse]
    let deleted: [String]
    let evict: [String]
    let newAnchor: String
}

struct ChangedEntryResponse {
    let relativePath: String
    let name: String
    let isDir: Bool
    let size: UInt64
    let modified: Double
    let created: Double

    init?(json: [String: Any]) {
        guard let relativePath = json["relativePath"] as? String,
              let name = json["name"] as? String else { return nil }
        self.relativePath = relativePath
        self.name = name
        self.isDir = json["isDir"] as? Bool ?? false
        self.size = json["size"] as? UInt64 ?? 0
        self.modified = json["modified"] as? Double ?? 0
        self.created = json["created"] as? Double ?? 0
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
