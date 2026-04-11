import FileProvider
import UniformTypeIdentifiers

/// Wraps a file/folder from the NAS as a FileProvider item.
/// Stores the SMB path so the extension can locate the file on the NAS mount.
class FileProviderItem: NSObject, NSFileProviderItem {
    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    let capabilities: NSFileProviderItemCapabilities
    let documentSize: NSNumber?
    let contentModificationDate: Date?
    let creationDate: Date?
    let itemVersion: NSFileProviderItemVersion

    /// The path on the SMB mount (/Volumes/{share}/relative/path).
    let smbPath: String

    init(
        identifier: NSFileProviderItemIdentifier,
        parentIdentifier: NSFileProviderItemIdentifier,
        filename: String,
        isDirectory: Bool,
        size: Int64?,
        modified: Date?,
        created: Date?,
        smbPath: String
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parentIdentifier
        self.filename = filename
        self.smbPath = smbPath

        if isDirectory {
            self.contentType = .folder
            self.capabilities = [.allowsReading, .allowsContentEnumerating]
        } else {
            let ext = (filename as NSString).pathExtension
            self.contentType = UTType(filenameExtension: ext) ?? .data
            self.capabilities = [.allowsReading]
        }

        self.documentSize = size.map { NSNumber(value: $0) }
        self.contentModificationDate = modified
        self.creationDate = created

        // Version based on modification date + size (same concept as Windows identity blob)
        let versionData = "\(modified?.timeIntervalSince1970 ?? 0):\(size ?? 0)".data(using: .utf8) ?? Data()
        self.itemVersion = NSFileProviderItemVersion(contentVersion: versionData, metadataVersion: versionData)
    }

    /// Create an item for the root container.
    static func rootContainer(displayName: String, smbPath: String) -> FileProviderItem {
        return FileProviderItem(
            identifier: .rootContainer,
            parentIdentifier: .rootContainer,
            filename: displayName,
            isDirectory: true,
            size: nil,
            modified: nil,
            created: nil,
            smbPath: smbPath
        )
    }
}
