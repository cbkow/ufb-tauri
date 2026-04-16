import Foundation

/// Observable model for a single mount's status.
struct MountInfo: Identifiable {
    let id: String
    let displayName: String
    var state: String
    var stateDetail: String
}
