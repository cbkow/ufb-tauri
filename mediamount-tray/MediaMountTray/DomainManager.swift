import Foundation
import FileProvider

/// Manages FileProvider domain registration for sync-enabled mounts.
/// Reads the agent's mounts.json config and registers/unregisters domains.
class DomainManager: ObservableObject {
    @Published var registeredDomains: [String] = []

    private let configPath: URL

    private var configMtime: Date?

    /// True when NFS loopback is the authoritative filesystem surface —
    /// in that world every `NSFileProviderDomain` registration is noise
    /// that produces a duplicate sidebar entry and potentially fights the
    /// NFS mount. Gate all registration on `UFB_ENABLE_NFS != 1`.
    private let disabledByNfs: Bool

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let path = home.appendingPathComponent(".local/share/ufb/mounts.json")
        configPath = path
        configMtime = try? FileManager.default
            .attributesOfItem(atPath: path.path)[.modificationDate] as? Date
        disabledByNfs = ProcessInfo.processInfo.environment["UFB_ENABLE_NFS"] == "1"

        if disabledByNfs {
            NSLog("[DomainManager] Disabled (UFB_ENABLE_NFS=1) — NFS owns the filesystem surface")
            cleanupLegacyDomainsOnce()
            return
        }

        registerDomains()
        startConfigWatcher()
    }

    /// One-shot cleanup: remove any FileProvider domains left over from a
    /// prior run that pre-dated NFS mode. Gated with a UserDefaults flag so
    /// it runs at most once per install; Slice 5 inherits the same flag.
    private func cleanupLegacyDomainsOnce() {
        let flagKey = "UFB.legacyDomainCleanupDone.v1"
        let defaults = UserDefaults.standard
        if defaults.bool(forKey: flagKey) {
            return
        }
        NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
            if let error = error {
                NSLog("[DomainManager] Legacy cleanup: failed to list domains: \(error)")
                return
            }
            let ufbDomains = (domains ?? []).filter { d in
                // UFB share names are the identifiers we registered with.
                // There is no stable "is this ours" marker other than the
                // presence of the domain — since NFS mode otherwise
                // registers zero domains, removing every reachable one on
                // this user's machine is safe.
                _ = d
                return true
            }
            if ufbDomains.isEmpty {
                defaults.set(true, forKey: flagKey)
                return
            }
            NSLog("[DomainManager] Legacy cleanup: removing \(ufbDomains.count) leftover domain(s)")
            let remaining = UnsafeMutablePointer<Int>.allocate(capacity: 1)
            remaining.pointee = ufbDomains.count
            for domain in ufbDomains {
                NSFileProviderManager.remove(domain) { err in
                    if let err = err {
                        NSLog("[DomainManager] Legacy cleanup: remove \(domain.identifier.rawValue) failed: \(err)")
                    }
                    remaining.pointee -= 1
                    if remaining.pointee == 0 {
                        defaults.set(true, forKey: flagKey)
                        remaining.deallocate()
                    }
                }
            }
        }
    }

    /// Poll config file for changes every 5 seconds.
    private func startConfigWatcher() {
        DispatchQueue.global(qos: .background).async { [weak self] in
            while true {
                Thread.sleep(forTimeInterval: 5.0)
                guard let self = self else { return }
                let currentMtime = self.configFileModified()
                if currentMtime != self.configMtime {
                    self.configMtime = currentMtime
                    NSLog("[DomainManager] Config file changed, re-registering domains")
                    self.registerDomains()
                }
            }
        }
    }

    private func configFileModified() -> Date? {
        try? FileManager.default.attributesOfItem(atPath: configPath.path)[.modificationDate] as? Date
    }

    /// Read mounts.json and register FileProvider domains for sync-enabled mounts.
    func registerDomains() {
        guard let config = loadConfig() else {
            NSLog("[DomainManager] No config or failed to load \(configPath.path)")
            return
        }

        // Register sync-enabled mounts as FileProvider domains.
        // Regular SMB mounts use direct /Volumes/ access (no FileProvider overhead/caching).
        let fpMounts = config.mounts.filter { $0.enabled && ($0.syncEnabled ?? false) }
        NSLog("[DomainManager] Found \(fpMounts.count) sync-enabled mounts: \(fpMounts.map { $0.shareName })")

        if fpMounts.isEmpty {
            NSLog("[DomainManager] No sync-enabled mounts")
            // Remove any stale domains
            NSFileProviderManager.getDomainsWithCompletionHandler { domains, _ in
                for domain in domains ?? [] {
                    NSFileProviderManager.remove(domain) { _ in }
                }
            }
            return
        }

        let desiredIds = Set(fpMounts.map { $0.shareName })

        // Get currently registered domains
        NSFileProviderManager.getDomainsWithCompletionHandler { existingDomains, error in
            if let error = error {
                NSLog("[DomainManager] Failed to list domains: \(error)")
            }

            let existing = existingDomains ?? []
            let existingIds = Set(existing.map { $0.identifier.rawValue })
            NSLog("[DomainManager] Existing domains: \(existingIds)")

            // Remove domains that shouldn't exist
            let toRemove = existing.filter { !desiredIds.contains($0.identifier.rawValue) }
            for domain in toRemove {
                NSLog("[DomainManager] Removing stale domain: \(domain.identifier.rawValue)")
                NSFileProviderManager.remove(domain) { error in
                    if let error = error {
                        NSLog("[DomainManager] Failed to remove \(domain.identifier.rawValue): \(error)")
                    }
                }
            }

            // Register missing domains
            for mount in fpMounts {
                let domainId = mount.shareName
                if existingIds.contains(domainId) {
                    NSLog("[DomainManager] Domain already registered: \(domainId)")
                    DispatchQueue.main.async {
                        if !self.registeredDomains.contains(domainId) {
                            self.registeredDomains.append(domainId)
                        }
                    }
                    continue
                }

                NSLog("[DomainManager] Registering domain: \(domainId) (\(mount.displayName))")
                let domain = NSFileProviderDomain(
                    identifier: NSFileProviderDomainIdentifier(rawValue: domainId),
                    displayName: mount.displayName
                )

                NSFileProviderManager.add(domain) { error in
                    if let error = error {
                        NSLog("[DomainManager] Failed to register \(domainId): \(error)")
                    } else {
                        NSLog("[DomainManager] Registered domain: \(domainId)")
                        DispatchQueue.main.async {
                            self.registeredDomains.append(domainId)
                        }
                    }
                }
            }
        }
    }

    // MARK: - Config parsing

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

        /// Extract the last path component from nasSharePath (mirrors Rust share_name()).
        var shareName: String {
            let trimmed = nasSharePath.trimmingCharacters(in: CharacterSet(charactersIn: "\\"))
            let parts = trimmed.split(separator: "\\").map(String.init)
            return parts.last ?? id
        }

        var isEnabled: Bool { enabled }
        var isSyncEnabled: Bool { syncEnabled ?? false }
    }

    private func loadConfig() -> MountsConfig? {
        guard FileManager.default.fileExists(atPath: configPath.path),
              let data = try? Data(contentsOf: configPath),
              let config = try? JSONDecoder().decode(MountsConfig.self, from: data) else {
            return nil
        }
        return config
    }
}
