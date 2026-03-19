#[cfg(windows)]
pub mod server;

#[cfg(target_os = "linux")]
pub mod unix_server;

use std::io::{self, Read, Write};

/// Write a length-prefixed JSON message.
/// Format: [4 bytes: u32 little-endian payload length][UTF-8 JSON payload]
pub fn write_message<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

/// Read a length-prefixed JSON message.
/// Returns the payload bytes, or an error if the pipe is broken/closed.
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    // Sanity check: reject messages > 16 MB
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes", len),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Send a typed message over a writer.
pub fn send_message<W: Write, T: serde::Serialize>(writer: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_message(writer, &payload)
}

/// Receive and deserialize a typed message from a reader.
pub fn recv_message<R: Read, T: serde::de::DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let payload = read_message(reader)?;
    serde_json::from_slice(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
