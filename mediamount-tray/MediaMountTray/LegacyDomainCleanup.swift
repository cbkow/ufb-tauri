import Foundation
import FileProvider

/// One-shot cleanup for upgrading installs. Before Slice 5 the tray
/// registered `NSFileProviderDomain` objects per sync-enabled mount;
/// those left behind Finder "Locations" sidebar entries that now shadow
/// our NFS mount points. This helper enumerates any domains UFB
/// previously registered with the system and removes them exactly once
/// per install (UserDefaults flag).
///
/// Lives as a free function because it's a fire-and-forget one-time
/// call made from `MediaMountTrayApp.init`. No state, no lifetime.
enum LegacyDomainCleanup {
    private static let flagKey = "UFB.legacyDomainCleanupDone.v1"

    static func runOnce() {
        let defaults = UserDefaults.standard
        if defaults.bool(forKey: flagKey) {
            return
        }
        NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
            if let error = error {
                NSLog("[LegacyDomainCleanup] getDomains failed: \(error)")
                return
            }
            // `getDomainsWithCompletionHandler` is scoped to THIS app's
            // bundle by macOS, so every returned domain was registered by
            // a prior UFB run. Safe to remove wholesale.
            let list = domains ?? []
            if list.isEmpty {
                defaults.set(true, forKey: flagKey)
                return
            }
            NSLog("[LegacyDomainCleanup] Removing \(list.count) legacy domain(s)")
            var remaining = list.count
            for domain in list {
                NSFileProviderManager.remove(domain) { err in
                    if let err = err {
                        NSLog(
                            "[LegacyDomainCleanup] remove \(domain.identifier.rawValue) failed: \(err)"
                        )
                    }
                    remaining -= 1
                    if remaining == 0 {
                        defaults.set(true, forKey: flagKey)
                    }
                }
            }
        }
    }
}
