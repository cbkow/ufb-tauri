// Windows drive mounting is now handled entirely by WNetAddConnection2W
// in fallback.rs (connect_drive / disconnect_drive).
//
// This module previously used DefineDosDevice for drive letter aliasing,
// which was needed for the old rclone fallback architecture. With SMB-only
// mounting, WNetAddConnection2W provides a proper network drive that shows
// in Explorer, `net use`, and auto-reconnects.
