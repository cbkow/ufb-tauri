import Cocoa
import FinderSync

/// Paints hydration-state overlay badges on files under `~/ufb/mounts/`,
/// adds a Finder toolbar button that opens the main UFB app, and vends a
/// context menu with share-level actions. Replaces the visual surface we
/// lost when we stopped registering `NSFileProviderDomain` objects.
///
/// Badge data flow: `BadgeClient` subscribes to the agent's Unix socket
/// and receives `BadgeUpdate` messages whenever a file's hydration state
/// changes. On each update it calls `setBadgeIdentifier(_:for:)` directly
/// — no `requestBadgeIdentifier` caching needed since the agent pushes
/// state proactively.
class FinderSyncController: FIFinderSync {
    static let badgeHydrated = "ufb.hydrated"
    static let badgePartial = "ufb.partial"

    private let badgeClient = BadgeClient()
    private let mountsRoot: URL = {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("ufb/mounts")
    }()

    override init() {
        super.init()
        NSLog("[UFB.FinderSync] Launched — monitoring \(mountsRoot.path)")

        // Register badge identifiers. macOS renders these at 16pt in the
        // Finder sidebar preview and 12–20pt as icon overlays.
        registerBadges()

        // Monitor the shared mounts root. FinderSync has a cap around ~20
        // URLs total; monitoring the common ancestor covers every current
        // and future share with a single slot.
        FIFinderSyncController.default().directoryURLs = [mountsRoot]

        // Wire up the push-model badge client. Updates arrive on a
        // background thread; forward to main before touching FIFinderSync.
        badgeClient.onBadgeChange = { [weak self] url, identifier in
            DispatchQueue.main.async {
                if let id = identifier {
                    FIFinderSyncController.default().setBadgeIdentifier(id, for: url)
                } else {
                    FIFinderSyncController.default().setBadgeIdentifier("", for: url)
                }
                _ = self  // silence unused-self warning
            }
        }
        badgeClient.resolvePathToURL = { [weak self] domain, relpath in
            guard let self = self else { return nil }
            return self.mountsRoot
                .appendingPathComponent(domain)
                .appendingPathComponent(relpath)
        }
        badgeClient.connect()
    }

    private func registerBadges() {
        let controller = FIFinderSyncController.default()
        if let hydrated = Self.makeBadgeImage(systemSymbol: "checkmark.circle.fill", color: .systemGreen) {
            controller.setBadgeImage(hydrated, label: "Cached locally", forBadgeIdentifier: Self.badgeHydrated)
        }
        if let partial = Self.makeBadgeImage(systemSymbol: "circle.lefthalf.filled", color: .systemBlue) {
            controller.setBadgeImage(partial, label: "Partially cached", forBadgeIdentifier: Self.badgePartial)
        }
    }

    /// Render an SF Symbol to a 16×16 NSImage tinted with the given color.
    /// Used in place of baked asset-catalog PDFs so we avoid ping-ponging
    /// binary assets through git for v1.
    private static func makeBadgeImage(systemSymbol: String, color: NSColor) -> NSImage? {
        let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
        guard let symbol = NSImage(systemSymbolName: systemSymbol, accessibilityDescription: nil)?
            .withSymbolConfiguration(config)
        else {
            return nil
        }
        let size = NSSize(width: 16, height: 16)
        let tinted = NSImage(size: size)
        tinted.lockFocus()
        color.set()
        let rect = NSRect(origin: .zero, size: size)
        symbol.draw(
            in: rect,
            from: .zero,
            operation: .sourceOver,
            fraction: 1.0,
            respectFlipped: true,
            hints: [.interpolation: NSImageInterpolation.high]
        )
        rect.fill(using: .sourceAtop)
        tinted.unlockFocus()
        tinted.isTemplate = false
        return tinted
    }

    // MARK: - Toolbar

    override var toolbarItemName: String { "UFB" }
    override var toolbarItemToolTip: String { "Open Union File Browser" }
    override var toolbarItemImage: NSImage {
        NSImage(systemSymbolName: "square.stack.3d.up", accessibilityDescription: "UFB")
            ?? NSImage()
    }

    override func menu(for menuKind: FIMenuKind) -> NSMenu {
        let menu = NSMenu(title: "")

        let openItem = NSMenuItem(title: "Open in UFB", action: #selector(openUfb(_:)), keyEquivalent: "")
        openItem.target = self
        menu.addItem(openItem)

        // Only offer drain when we're on a share root (direct child of
        // ~/ufb/mounts). Deeper paths wouldn't map to a single mount id,
        // and drain is a share-level action.
        if menuKind == .contextualMenuForItems, shareRootForCurrentSelection() != nil {
            let drainItem = NSMenuItem(
                title: "Drain Cache for this Share",
                action: #selector(drainCache(_:)),
                keyEquivalent: ""
            )
            drainItem.target = self
            menu.addItem(drainItem)
        }

        return menu
    }

    /// If the current Finder selection resolves to a single share root
    /// (`~/ufb/mounts/<share>`), return its share name. Otherwise nil.
    private func shareRootForCurrentSelection() -> String? {
        guard let selected = FIFinderSyncController.default().selectedItemURLs(),
            selected.count == 1,
            let url = selected.first
        else { return nil }

        let rootComponents = mountsRoot.standardizedFileURL.pathComponents
        let urlComponents = url.standardizedFileURL.pathComponents
        // Need exactly one more component than mountsRoot — that's the share.
        guard urlComponents.count == rootComponents.count + 1,
            urlComponents.dropLast() == rootComponents[...]
        else {
            return nil
        }
        return urlComponents.last
    }

    @objc private func openUfb(_ sender: AnyObject?) {
        let candidates = [
            "/Applications/UFB.app",
            "/Applications/Union File Browser.app",
        ]
        for path in candidates {
            if FileManager.default.fileExists(atPath: path) {
                NSWorkspace.shared.open(URL(fileURLWithPath: path))
                return
            }
        }
    }

    @objc private func drainCache(_ sender: AnyObject?) {
        guard let share = shareRootForCurrentSelection() else { return }
        badgeClient.drainShareCache(mountId: share)
    }
}
