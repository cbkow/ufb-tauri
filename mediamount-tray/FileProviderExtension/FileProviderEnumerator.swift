import FileProvider
import UniformTypeIdentifiers

/// Enumerates directory contents from the NAS via IPC to the mediamount-agent.
/// The agent reads the actual SMB mount and returns results over the socket.
class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let enumeratedItemIdentifier: NSFileProviderItemIdentifier
    private let domainId: String  // share_name (e.g., "test1")

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, domainId: String) {
        self.enumeratedItemIdentifier = enumeratedItemIdentifier
        self.domainId = domainId
        super.init()
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        // Working set and trash are virtual containers — return empty
        if enumeratedItemIdentifier == .workingSet || enumeratedItemIdentifier == .trashContainer {
            observer.didEnumerate([])
            observer.finishEnumerating(upTo: nil)
            return
        }

        let relativePath: String
        if enumeratedItemIdentifier == .rootContainer {
            relativePath = ""
        } else {
            relativePath = enumeratedItemIdentifier.rawValue
        }

        NSLog("[FileProviderEnumerator] Listing domain=%@ path=%@", domainId, relativePath.isEmpty ? "(root)" : relativePath)

        do {
            let entries = try AgentFileOpsClient.shared.listDir(domain: domainId, relativePath: relativePath)

            let items: [NSFileProviderItem] = entries.map { entry in
                let itemRelativePath: String
                if relativePath.isEmpty {
                    itemRelativePath = entry.name
                } else {
                    itemRelativePath = relativePath + "/" + entry.name
                }

                return FileProviderItem(
                    identifier: NSFileProviderItemIdentifier(rawValue: itemRelativePath),
                    parentIdentifier: enumeratedItemIdentifier,
                    filename: entry.name,
                    isDirectory: entry.isDir,
                    size: Int64(entry.size),
                    modified: Date(timeIntervalSince1970: entry.modified),
                    created: Date(timeIntervalSince1970: entry.created),
                    smbPath: ""  // not used with IPC
                )
            }

            NSLog("[FileProviderEnumerator] Found %d items", items.count)
            observer.didEnumerate(items)
            observer.finishEnumerating(upTo: nil)
        } catch {
            NSLog("[FileProviderEnumerator] ERROR: %@", error.localizedDescription)
            let nsError = (error as? FileOpsError)?.asNSError ?? (error as NSError)
            observer.finishEnumeratingWithError(nsError)
        }
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        // No incremental change tracking yet — system falls back to full enumeration
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        let anchor = NSFileProviderSyncAnchor(rawValue: Data(Date().description.utf8))
        completionHandler(anchor)
    }
}
