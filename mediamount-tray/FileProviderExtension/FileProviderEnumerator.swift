import FileProvider

/// Enumerates directory contents from the NAS via the SMB mount at /Volumes/.
/// This is the spike implementation — reads the mounted SMB share directly.
class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let enumeratedItemIdentifier: NSFileProviderItemIdentifier
    private let nasBasePath: String  // /Volumes/{shareName}

    init(enumeratedItemIdentifier: NSFileProviderItemIdentifier, nasBasePath: String) {
        self.enumeratedItemIdentifier = enumeratedItemIdentifier
        self.nasBasePath = nasBasePath
        super.init()
    }

    func invalidate() {
        // Nothing to clean up in spike
    }

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        // Determine the directory path to list
        let directoryPath: String
        if enumeratedItemIdentifier == .rootContainer {
            directoryPath = nasBasePath
        } else {
            // Item identifier encodes the relative path from NAS root
            directoryPath = nasBasePath + "/" + enumeratedItemIdentifier.rawValue
        }

        NSLog("[FileProviderEnumerator] Listing: \(directoryPath)")

        let fm = FileManager.default
        var items: [NSFileProviderItem] = []

        do {
            let contents = try fm.contentsOfDirectory(atPath: directoryPath)

            for name in contents {
                // Skip hidden/system files
                if name.hasPrefix(".") || name.hasPrefix("@") || name.hasPrefix("#") {
                    continue
                }

                let fullPath = directoryPath + "/" + name

                // Build relative path from NAS root for the identifier
                let relativePath: String
                if enumeratedItemIdentifier == .rootContainer {
                    relativePath = name
                } else {
                    relativePath = enumeratedItemIdentifier.rawValue + "/" + name
                }

                do {
                    let attrs = try fm.attributesOfItem(atPath: fullPath)
                    let isDir = (attrs[.type] as? FileAttributeType) == .typeDirectory
                    let size = attrs[.size] as? Int64
                    let modified = attrs[.modificationDate] as? Date
                    let created = attrs[.creationDate] as? Date

                    let item = FileProviderItem(
                        identifier: NSFileProviderItemIdentifier(rawValue: relativePath),
                        parentIdentifier: enumeratedItemIdentifier,
                        filename: name,
                        isDirectory: isDir,
                        size: size,
                        modified: modified,
                        created: created,
                        smbPath: fullPath
                    )
                    items.append(item)
                } catch {
                    NSLog("[FileProviderEnumerator] Skipping \(name): \(error)")
                }
            }

            NSLog("[FileProviderEnumerator] Found \(items.count) items in \(directoryPath)")
        } catch {
            NSLog("[FileProviderEnumerator] ERROR listing \(directoryPath): \(error)")
            observer.finishEnumeratingWithError(error)
            return
        }

        observer.didEnumerate(items)
        observer.finishEnumerating(upTo: nil)
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        // Spike: no incremental change tracking yet.
        // Return empty changes — system will fall back to full enumeration.
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // Spike: use a timestamp as the anchor
        let anchor = NSFileProviderSyncAnchor(rawValue: Data(Date().description.utf8))
        completionHandler(anchor)
    }
}
