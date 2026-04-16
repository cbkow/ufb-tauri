import Foundation
import AppKit
import Combine
import CoreServices

/// Places UFB mount paths (`~/ufb/mounts/{shareName}`) in the user's Finder
/// sidebar under Favorites. Replaces the first-class sidebar entry we lost
/// when we stopped registering `NSFileProviderDomain` objects in NFS mode.
///
/// Only runs under `UFB_ENABLE_NFS=1`. When FileProvider is active the
/// domain-registered entry under "Locations" is already there; a parallel
/// Favorites entry would be a duplicate.
///
/// LSSharedFileList* is formally deprecated since 10.11 but still works on
/// Sonoma + Sequoia. No modern replacement exists for programmatic sidebar
/// insertion; Apple funnels this behavior exclusively through FileProvider
/// domains. If Apple removes the API entirely, the fallback is "drag the
/// folder into the sidebar once" documented to users.
class SidebarManager: ObservableObject {
    private weak var agent: AgentConnection?
    private var cancellables = Set<AnyCancellable>()
    private let configPath: URL
    private let mountsRoot: URL
    /// Reconcile only under NFS mode — FileProvider's own sidebar entries
    /// would collide otherwise.
    private let enabled: Bool

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        configPath = home.appendingPathComponent(".local/share/ufb/mounts.json")
        mountsRoot = home.appendingPathComponent("ufb/mounts")
        enabled = ProcessInfo.processInfo.environment["UFB_ENABLE_NFS"] == "1"
    }

    /// Wire up against the agent connection. Reconciles whenever the
    /// agent's mount set changes (and the agent is connected — an empty
    /// `mounts` array during a transient disconnect should not wipe our
    /// sidebar entries).
    func attach(to agent: AgentConnection) {
        guard enabled else {
            NSLog("[SidebarManager] Disabled (UFB_ENABLE_NFS != 1)")
            return
        }
        self.agent = agent
        agent.$mounts
            .combineLatest(agent.$connected)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] (_: [MountInfo], connected: Bool) in
                guard let self = self, connected else { return }
                self.reconcile()
            }
            .store(in: &cancellables)
    }

    /// Diff the desired set of sidebar entries against the current ones we
    /// own and apply the delta. We "own" an entry iff its resolved URL
    /// lives under `~/ufb/mounts/` — never touch user-added favorites.
    private func reconcile() {
        let desired = loadDesiredEntries()
        guard let list = LSSharedFileListCreate(
            nil,
            kLSSharedFileListFavoriteItems.takeUnretainedValue(),
            nil
        )?.takeRetainedValue() else {
            NSLog("[SidebarManager] Failed to open Favorites list")
            return
        }

        let existing = snapshotOwnedEntries(list: list)

        // Additions + updates (displayName changed → remove + re-add)
        var existingByPath: [String: (item: LSSharedFileListItem, name: String)] = [:]
        for (item, url, name) in existing {
            existingByPath[url.path] = (item, name)
        }

        for entry in desired {
            if let current = existingByPath[entry.url.path] {
                if current.name != entry.displayName {
                    LSSharedFileListItemRemove(list, current.item)
                    insertItem(list: list, entry: entry)
                }
                existingByPath.removeValue(forKey: entry.url.path)
            } else {
                insertItem(list: list, entry: entry)
            }
        }

        // Leftovers in existingByPath are orphans — mounts we used to have
        // but don't anymore. Remove them.
        for (_, current) in existingByPath {
            LSSharedFileListItemRemove(list, current.item)
        }

        NSLog(
            "[SidebarManager] Reconciled: \(desired.count) desired, removed \(existingByPath.count) orphans"
        )
    }

    private struct SidebarEntry {
        let url: URL
        let displayName: String
    }

    private func loadDesiredEntries() -> [SidebarEntry] {
        guard let agent = agent else { return [] }
        guard let config = loadConfig() else { return [] }
        var byId: [String: MountConfig] = [:]
        for m in config.mounts where m.enabled {
            byId[m.id] = m
        }
        // Only include mounts the agent currently reports (so the sidebar
        // reflects live state, not just config presence).
        return agent.mounts.compactMap { info -> SidebarEntry? in
            guard let cfg = byId[info.id] else { return nil }
            let url = mountsRoot.appendingPathComponent(cfg.shareName, isDirectory: true)
            let name = cfg.displayName.isEmpty ? cfg.shareName : cfg.displayName
            return SidebarEntry(url: url, displayName: name)
        }
    }

    /// Enumerate the user's current Favorites and return only the ones we
    /// own — URLs that resolve to a path under `~/ufb/mounts/`.
    private func snapshotOwnedEntries(list: LSSharedFileList)
        -> [(LSSharedFileListItem, URL, String)]
    {
        var seed: UInt32 = 0
        guard let snapshot = LSSharedFileListCopySnapshot(list, &seed)?.takeRetainedValue()
        else {
            return []
        }
        let rootPath = mountsRoot.path
        let count = CFArrayGetCount(snapshot)
        var owned: [(LSSharedFileListItem, URL, String)] = []
        for i in 0..<count {
            let raw = CFArrayGetValueAtIndex(snapshot, i)
            let item = Unmanaged<LSSharedFileListItem>.fromOpaque(raw!).takeUnretainedValue()
            guard let resolved = LSSharedFileListItemCopyResolvedURL(item, 0, nil)?
                .takeRetainedValue() as URL?
            else { continue }
            // Only touch entries whose file:// path sits inside our root.
            // Favorites may also hold smb:// or other schemes — skip those.
            if resolved.isFileURL, resolved.path.hasPrefix(rootPath + "/") || resolved.path == rootPath {
                let name = LSSharedFileListItemCopyDisplayName(item).takeRetainedValue() as String
                owned.append((item, resolved, name))
            }
        }
        return owned
    }

    private func insertItem(list: LSSharedFileList, entry: SidebarEntry) {
        // Pass nil for icon — macOS uses the default folder icon at
        // sidebar size (16pt). Supplying a custom icon via the legacy
        // IconRef API is gnarly and the branding value at 16pt is low;
        // revisit if users ask for it.
        _ = LSSharedFileListInsertItemURL(
            list,
            kLSSharedFileListItemLast.takeUnretainedValue(),
            entry.displayName as CFString,
            nil,
            entry.url as CFURL,
            nil,
            nil
        )
    }

    // MARK: - Config (subset, mirrors DomainManager's local decoder)

    private struct MountsConfig: Decodable {
        let version: Int
        let mounts: [MountConfig]
    }

    private struct MountConfig: Decodable {
        let id: String
        let enabled: Bool
        let displayName: String
        let nasSharePath: String
        let syncEnabled: Bool?

        var shareName: String {
            let trimmed = nasSharePath.trimmingCharacters(in: CharacterSet(charactersIn: "\\"))
            let parts = trimmed.split(separator: "\\").map(String.init)
            return parts.last ?? id
        }
    }

    private func loadConfig() -> MountsConfig? {
        guard FileManager.default.fileExists(atPath: configPath.path),
            let data = try? Data(contentsOf: configPath),
            let config = try? JSONDecoder().decode(MountsConfig.self, from: data)
        else {
            return nil
        }
        return config
    }
}
