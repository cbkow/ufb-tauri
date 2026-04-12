import FileProvider
import UniformTypeIdentifiers

/// NSFileProviderReplicatedExtension that proxies all file operations through
/// IPC to the mediamount-agent. The agent has /Volumes/ access; the extension
/// is sandboxed and cannot read SMB mounts directly.
class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain

    /// The domain identifier is the share_name (e.g., "test1").
    private var domainId: String {
        return domain.identifier.rawValue
    }

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        NSLog("[FileProvider] Extension initialized for domain: \(domainId)")
        registerForNasChangeNotifications()

        // Signal working set on init to catch up with any changes since last run
        DispatchQueue.global().asyncAfter(deadline: .now() + 1.0) {
            NSLog("[FileProvider] Initial working set signal for \(self.domainId)")
            NSFileProviderManager(for: self.domain)?.signalEnumerator(for: .workingSet) { error in
                if let error = error {
                    NSLog("[FileProvider] Initial signal error: \(error)")
                }
            }
        }
    }

    func invalidate() {
        NSLog("[FileProvider] Extension invalidated for domain: \(domainId)")
        DistributedNotificationCenter.default().removeObserver(self)
    }

    /// Listen for distributed notifications from the agent when NAS contents change.
    private func registerForNasChangeNotifications() {
        let notifName = "com.unionfiles.ufb.nas-changed.\(domainId)"
        NSLog("[FileProvider] Registering for notifications: \(notifName)")

        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(nasDidChange(_:)),
            name: NSNotification.Name(notifName),
            object: nil,
            suspensionBehavior: .deliverImmediately
        )

        // Listen for "clear cache" — evict all materialized files
        let clearName = "com.unionfiles.ufb.clear-cache.\(domainId)"
        NSLog("[FileProvider] Registering for clear-cache: \(clearName)")
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(clearCacheRequested(_:)),
            name: NSNotification.Name(clearName),
            object: nil,
            suspensionBehavior: .deliverImmediately
        )
    }

    @objc private func clearCacheRequested(_ notification: Notification) {
        NSLog("[FileProvider] Clear cache requested for \(domainId)")
        guard let manager = NSFileProviderManager(for: domain) else { return }

        // Get all items via a fresh listing and evict each one
        do {
            let entries = try AgentFileOpsClient.shared.listDir(domain: domainId, relativePath: "")
            var evictCount = 0
            for entry in entries where !entry.isDir {
                let identifier = NSFileProviderItemIdentifier(rawValue: entry.name)
                manager.evictItem(identifier: identifier) { error in
                    if let error = error {
                        NSLog("[FileProvider] evict error: \(error.localizedDescription)")
                    }
                }
                evictCount += 1
            }
            NSLog("[FileProvider] Evicting \(evictCount) files for \(domainId)")
        } catch {
            NSLog("[FileProvider] Clear cache listing failed: \(error.localizedDescription)")
        }
    }

    @objc private func nasDidChange(_ notification: Notification) {
        NSLog("[FileProvider] NAS change notification received for \(domainId)")
        // MUST signal .workingSet — signaling .rootContainer is silently ignored by the system
        NSFileProviderManager(for: domain)?.signalEnumerator(for: .workingSet) { error in
            if let error = error {
                NSLog("[FileProvider] signalEnumerator error: \(error)")
            } else {
                NSLog("[FileProvider] signalEnumerator(.workingSet) succeeded for \(self.domainId)")
            }
        }
    }

    // MARK: - NSFileProviderReplicatedExtension

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        NSLog("[FileProvider] item(for: \(identifier.rawValue))")

        if identifier == .rootContainer {
            completionHandler(
                FileProviderItem.rootContainer(displayName: domain.displayName, smbPath: ""),
                nil
            )
            return Progress()
        }

        let relativePath = identifier.rawValue
        let name = (relativePath as NSString).lastPathComponent
        let parentPath = (relativePath as NSString).deletingLastPathComponent
        let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
            ? .rootContainer
            : NSFileProviderItemIdentifier(rawValue: parentPath)

        do {
            let stat = try AgentFileOpsClient.shared.stat(domain: domainId, relativePath: relativePath)
            let item = FileProviderItem(
                identifier: identifier,
                parentIdentifier: parentId,
                filename: name,
                isDirectory: stat.isDir,
                size: Int64(stat.size),
                modified: Date(timeIntervalSince1970: stat.modified),
                created: Date(timeIntervalSince1970: stat.created),
                smbPath: ""
            )
            completionHandler(item, nil)
        } catch {
            NSLog("[FileProvider] ERROR stat \(relativePath): \(error.localizedDescription)")
            completionHandler(nil, NSFileProviderError(.noSuchItem))
        }

        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        NSLog("[FileProvider] enumerator(for: \(containerItemIdentifier.rawValue))")
        return FileProviderEnumerator(
            enumeratedItemIdentifier: containerItemIdentifier,
            domainId: domainId,
            domain: domain
        )
    }

    /// Called when a user opens/accesses a file.
    /// Agent copies the file to a temp path in the app group container.
    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let relativePath = itemIdentifier.rawValue
        NSLog("[FileProvider] fetchContents(for: \(relativePath))")

        do {
            let (tempURL, stat) = try AgentFileOpsClient.shared.readFile(
                domain: domainId,
                relativePath: relativePath
            )

            NSLog("[FileProvider] File ready at \(tempURL.path) (\(stat.size) bytes)")

            let name = (relativePath as NSString).lastPathComponent
            let parentPath = (relativePath as NSString).deletingLastPathComponent
            let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
                ? .rootContainer
                : NSFileProviderItemIdentifier(rawValue: parentPath)

            let item = FileProviderItem(
                identifier: itemIdentifier,
                parentIdentifier: parentId,
                filename: name,
                isDirectory: false,
                size: Int64(stat.size),
                modified: Date(timeIntervalSince1970: stat.modified),
                created: Date(timeIntervalSince1970: stat.created),
                smbPath: ""
            )

            completionHandler(tempURL, item, nil)
        } catch {
            NSLog("[FileProvider] ERROR fetchContents \(relativePath): \(error.localizedDescription)")
            completionHandler(nil, nil, (error as? FileOpsError)?.asNSError ?? (error as NSError))
        }

        return Progress()
    }

    // MARK: - Write operations

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let filename = itemTemplate.filename
        let parentId = itemTemplate.parentItemIdentifier
        let isDir = itemTemplate.contentType == .folder

        // Build the relative path for the new item
        let relativePath: String
        if parentId == .rootContainer {
            relativePath = filename
        } else {
            relativePath = parentId.rawValue + "/" + filename
        }

        NSLog("[FileProvider] createItem: \(relativePath) isDir=\(isDir)")

        do {
            let (size, modified) = try AgentFileOpsClient.shared.writeFile(
                domain: domainId,
                relativePath: relativePath,
                sourceURL: url,
                isDir: isDir
            )

            let item = FileProviderItem(
                identifier: NSFileProviderItemIdentifier(rawValue: relativePath),
                parentIdentifier: parentId,
                filename: filename,
                isDirectory: isDir,
                size: Int64(size),
                modified: Date(timeIntervalSince1970: modified),
                created: Date(timeIntervalSince1970: modified),
                smbPath: ""
            )

            completionHandler(item, [], false, nil)
        } catch {
            NSLog("[FileProvider] ERROR createItem \(relativePath): \(error.localizedDescription)")
            completionHandler(nil, [], false, (error as? FileOpsError)?.asNSError ?? (error as NSError))
        }

        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let oldRelativePath = item.itemIdentifier.rawValue
        NSLog("[FileProvider] modifyItem: \(oldRelativePath) fields=\(changedFields)")

        do {
            // Check for rename
            if changedFields.contains(.filename) {
                let newFilename = item.filename
                let parentPath = (oldRelativePath as NSString).deletingLastPathComponent
                let newRelativePath = parentPath.isEmpty ? newFilename : parentPath + "/" + newFilename
                let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
                    ? .rootContainer
                    : NSFileProviderItemIdentifier(rawValue: parentPath)

                NSLog("[FileProvider] Rename: \(oldRelativePath) → \(newRelativePath)")

                let (size, modified) = try AgentFileOpsClient.shared.renameItem(
                    domain: domainId,
                    oldPath: oldRelativePath,
                    newPath: newRelativePath
                )

                let isDir = item.contentType == .folder
                let renamedItem = FileProviderItem(
                    identifier: NSFileProviderItemIdentifier(rawValue: newRelativePath),
                    parentIdentifier: parentId,
                    filename: newFilename,
                    isDirectory: isDir,
                    size: Int64(size),
                    modified: Date(timeIntervalSince1970: modified),
                    created: item.creationDate ?? nil,
                    smbPath: ""
                )

                completionHandler(renamedItem, [], false, nil)
                return Progress()
            }

            // Content change
            if let contentsURL = newContents {
                let (size, modified) = try AgentFileOpsClient.shared.writeFile(
                    domain: domainId,
                    relativePath: oldRelativePath,
                    sourceURL: contentsURL
                )

                let parentPath = (oldRelativePath as NSString).deletingLastPathComponent
                let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
                    ? .rootContainer
                    : NSFileProviderItemIdentifier(rawValue: parentPath)

                let updatedItem = FileProviderItem(
                    identifier: item.itemIdentifier,
                    parentIdentifier: parentId,
                    filename: item.filename,
                    isDirectory: false,
                    size: Int64(size),
                    modified: Date(timeIntervalSince1970: modified),
                    created: item.creationDate ?? nil,
                    smbPath: ""
                )

                completionHandler(updatedItem, [], false, nil)
            } else {
                // No content change, no rename — just acknowledge
                completionHandler(item, [], false, nil)
            }
        } catch {
            NSLog("[FileProvider] ERROR modifyItem \(oldRelativePath): \(error.localizedDescription)")
            completionHandler(nil, [], false, (error as? FileOpsError)?.asNSError ?? (error as NSError))
        }

        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let relativePath = identifier.rawValue
        NSLog("[FileProvider] deleteItem: \(relativePath)")

        do {
            try AgentFileOpsClient.shared.deleteItem(domain: domainId, relativePath: relativePath)
            completionHandler(nil)
        } catch {
            NSLog("[FileProvider] ERROR deleteItem \(relativePath): \(error.localizedDescription)")
            completionHandler((error as? FileOpsError)?.asNSError ?? (error as NSError))
        }

        return Progress()
    }
}
