import FileProvider
import UniformTypeIdentifiers

/// Enumerates directory contents from the NAS via IPC to the mediamount-agent.
class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let enumeratedItemIdentifier: NSFileProviderItemIdentifier
    private let domainId: String

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, domainId: String) {
        self.enumeratedItemIdentifier = enumeratedItemIdentifier
        self.domainId = domainId
        super.init()
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        if enumeratedItemIdentifier == .trashContainer {
            observer.didEnumerate([])
            observer.finishEnumerating(upTo: nil)
            return
        }

        let relativePath: String
        if enumeratedItemIdentifier == .rootContainer || enumeratedItemIdentifier == .workingSet {
            relativePath = ""
        } else {
            relativePath = enumeratedItemIdentifier.rawValue
        }

        let parentId = (enumeratedItemIdentifier == .workingSet)
            ? NSFileProviderItemIdentifier.rootContainer
            : enumeratedItemIdentifier

        NSLog("[Enumerator] enumerateItems container=%@ domain=%@ path=%@",
              enumeratedItemIdentifier.rawValue, domainId,
              relativePath.isEmpty ? "(root)" : relativePath)

        do {
            let items = try fetchItems(relativePath: relativePath, parentId: parentId)
            NSLog("[Enumerator] Found %d items", items.count)
            observer.didEnumerate(items)
            observer.finishEnumerating(upTo: nil)
        } catch {
            NSLog("[Enumerator] ERROR: %@", error.localizedDescription)
            observer.finishEnumeratingWithError((error as? FileOpsError)?.asNSError ?? (error as NSError))
        }
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        if enumeratedItemIdentifier == .trashContainer {
            observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
            return
        }

        NSLog("[Enumerator] enumerateChanges container=%@ domain=%@",
              enumeratedItemIdentifier.rawValue, domainId)

        if enumeratedItemIdentifier == .workingSet {
            // Working set: use the agent's DB-backed change detection
            enumerateWorkingSetChanges(observer: observer, anchor: anchor)
        } else {
            // Specific folder: always do a fresh listing (same as Windows FETCH_PLACEHOLDERS)
            enumerateFolderChanges(observer: observer, anchor: anchor)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        let ts = Date().timeIntervalSince1970
        let anchor = NSFileProviderSyncAnchor(rawValue: "\(ts)".data(using: .utf8)!)
        completionHandler(anchor)
    }

    // MARK: - Private

    /// Fetch items from the agent via IPC. Used by both enumerateItems and folder-level enumerateChanges.
    private func fetchItems(relativePath: String, parentId: NSFileProviderItemIdentifier) throws -> [NSFileProviderItem] {
        let entries = try AgentFileOpsClient.shared.listDir(domain: domainId, relativePath: relativePath)

        return entries.map { entry in
            let itemRelativePath = relativePath.isEmpty ? entry.name : relativePath + "/" + entry.name
            return FileProviderItem(
                identifier: NSFileProviderItemIdentifier(rawValue: itemRelativePath),
                parentIdentifier: parentId,
                filename: entry.name,
                isDirectory: entry.isDir,
                size: Int64(entry.size),
                modified: Date(timeIntervalSince1970: entry.modified),
                created: Date(timeIntervalSince1970: entry.created),
                smbPath: ""
            )
        }
    }

    /// For specific folders: do a fresh listDir and report all items as updates.
    /// The system diffs against its cache. This matches Windows FETCH_PLACEHOLDERS behavior.
    private func enumerateFolderChanges(observer: NSFileProviderChangeObserver, anchor: NSFileProviderSyncAnchor) {
        let relativePath = enumeratedItemIdentifier == .rootContainer ? "" : enumeratedItemIdentifier.rawValue

        do {
            let items = try fetchItems(relativePath: relativePath, parentId: enumeratedItemIdentifier)

            if !items.isEmpty {
                observer.didUpdate(items)
            }

            let newAnchor = NSFileProviderSyncAnchor(rawValue: "\(Date().timeIntervalSince1970)".data(using: .utf8)!)
            observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
        } catch {
            NSLog("[Enumerator] enumerateFolderChanges error: %@", error.localizedDescription)
            observer.finishEnumeratingWithError(NSFileProviderError(.syncAnchorExpired))
        }
    }

    /// For working set: use the agent's DB-backed getChanges for efficient cold start catch-up.
    private func enumerateWorkingSetChanges(observer: NSFileProviderChangeObserver, anchor: NSFileProviderSyncAnchor) {
        let anchorString = String(data: anchor.rawValue, encoding: .utf8) ?? "0"

        do {
            let changes = try AgentFileOpsClient.shared.getChanges(domain: domainId, sinceAnchor: anchorString)

            NSLog("[Enumerator] WorkingSet changes: %d updated, %d deleted", changes.updated.count, changes.deleted.count)

            if !changes.updated.isEmpty {
                let items: [NSFileProviderItem] = changes.updated.map { entry in
                    let parentPath = (entry.relativePath as NSString).deletingLastPathComponent
                    let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
                        ? .rootContainer
                        : NSFileProviderItemIdentifier(rawValue: parentPath)

                    return FileProviderItem(
                        identifier: NSFileProviderItemIdentifier(rawValue: entry.relativePath),
                        parentIdentifier: parentId,
                        filename: entry.name,
                        isDirectory: entry.isDir,
                        size: Int64(entry.size),
                        modified: Date(timeIntervalSince1970: entry.modified),
                        created: Date(timeIntervalSince1970: entry.created),
                        smbPath: ""
                    )
                }
                observer.didUpdate(items)
            }

            if !changes.deleted.isEmpty {
                let deletedIds = changes.deleted.map { NSFileProviderItemIdentifier(rawValue: $0) }
                observer.didDeleteItems(withIdentifiers: deletedIds)
            }

            let newAnchor = NSFileProviderSyncAnchor(rawValue: changes.newAnchor.data(using: .utf8) ?? Data())
            observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
        } catch {
            NSLog("[Enumerator] enumerateWorkingSetChanges error: %@", error.localizedDescription)
            observer.finishEnumeratingWithError(NSFileProviderError(.syncAnchorExpired))
        }
    }
}
