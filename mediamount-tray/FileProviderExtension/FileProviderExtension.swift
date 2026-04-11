import FileProvider
import UniformTypeIdentifiers

/// Minimal NSFileProviderReplicatedExtension for the Phase 0 spike.
///
/// Architecture: The extension reads NAS files directly via the SMB mount at /Volumes/.
/// The agent mounts the SMB share headlessly, and this extension accesses the files
/// through the standard filesystem — same pattern as Windows CF API using std::fs on UNC paths.
///
/// If the sandbox blocks /Volumes/ access, we'll fall back to IPC-based file operations.
class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain

    /// The NAS mount path. Domain identifier is the share_name (e.g., "test1"),
    /// and the SMB mount lives at /Volumes/{share_name}.
    private var nasBasePath: String {
        return "/Volumes/\(domain.identifier.rawValue)"
    }

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        NSLog("[FileProvider] Extension initialized for domain: \(domain.identifier.rawValue)")
        NSLog("[FileProvider] NAS base path: \(nasBasePath)")
    }

    func invalidate() {
        NSLog("[FileProvider] Extension invalidated for domain: \(domain.identifier.rawValue)")
    }

    // MARK: - NSFileProviderReplicatedExtension

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        NSLog("[FileProvider] item(for: \(identifier.rawValue))")

        if identifier == .rootContainer {
            completionHandler(FileProviderItem.rootContainer(displayName: domain.displayName, smbPath: nasBasePath), nil)
            return Progress()
        }

        // Item identifier is the relative path from NAS root
        let fullPath = nasBasePath + "/" + identifier.rawValue

        let fm = FileManager.default
        do {
            let attrs = try fm.attributesOfItem(atPath: fullPath)
            let name = (identifier.rawValue as NSString).lastPathComponent
            let parentPath = (identifier.rawValue as NSString).deletingLastPathComponent
            let parentId: NSFileProviderItemIdentifier = parentPath.isEmpty
                ? .rootContainer
                : NSFileProviderItemIdentifier(rawValue: parentPath)

            let isDir = (attrs[.type] as? FileAttributeType) == .typeDirectory
            let size = attrs[.size] as? Int64
            let modified = attrs[.modificationDate] as? Date
            let created = attrs[.creationDate] as? Date

            let item = FileProviderItem(
                identifier: identifier,
                parentIdentifier: parentId,
                filename: name,
                isDirectory: isDir,
                size: size,
                modified: modified,
                created: created,
                smbPath: fullPath
            )
            completionHandler(item, nil)
        } catch {
            NSLog("[FileProvider] ERROR item lookup \(fullPath): \(error)")
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
            nasBasePath: nasBasePath
        )
    }

    /// Called when a user opens/accesses a file. This is the SANDBOX TEST.
    /// If this succeeds, direct /Volumes/ access works from the extension sandbox.
    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        NSLog("[FileProvider] fetchContents(for: \(itemIdentifier.rawValue))")

        let sourcePath = nasBasePath + "/" + itemIdentifier.rawValue
        NSLog("[FileProvider] Reading from SMB: \(sourcePath)")

        // Copy file from NAS mount to a temporary location that FileProvider can serve
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileProvider-\(domain.identifier.rawValue)", isDirectory: true)

        do {
            try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        } catch {
            NSLog("[FileProvider] ERROR creating temp dir: \(error)")
            completionHandler(nil, nil, error)
            return Progress()
        }

        let filename = (itemIdentifier.rawValue as NSString).lastPathComponent
        let tempFile = tempDir.appendingPathComponent(filename)

        // Remove old temp copy if exists
        try? FileManager.default.removeItem(at: tempFile)

        do {
            // THIS IS THE SANDBOX TEST — can we read from /Volumes/?
            try FileManager.default.copyItem(
                atPath: sourcePath,
                toPath: tempFile.path
            )
            NSLog("[FileProvider] SUCCESS: Copied \(sourcePath) → \(tempFile.path)")

            // Return the item metadata alongside the file URL
            let attrs = try FileManager.default.attributesOfItem(atPath: sourcePath)
            let item = FileProviderItem(
                identifier: itemIdentifier,
                parentIdentifier: .rootContainer, // simplified for spike
                filename: filename,
                isDirectory: false,
                size: attrs[.size] as? Int64,
                modified: attrs[.modificationDate] as? Date,
                created: attrs[.creationDate] as? Date,
                smbPath: sourcePath
            )

            completionHandler(tempFile, item, nil)
        } catch {
            NSLog("[FileProvider] SANDBOX TEST FAILED or file error: \(error)")
            completionHandler(nil, nil, error)
        }

        return Progress()
    }

    // MARK: - Stub methods (not implemented in spike)

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        NSLog("[FileProvider] createItem — NOT IMPLEMENTED (spike)")
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
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
        NSLog("[FileProvider] modifyItem — NOT IMPLEMENTED (spike)")
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        NSLog("[FileProvider] deleteItem — NOT IMPLEMENTED (spike)")
        completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }
}
