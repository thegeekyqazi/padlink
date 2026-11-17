//! usbmuxd message framing.
//!
//! Every message is a 16-byte little-endian header followed by an XML
//! property list:
//!
//! ```text
//! [u32 total length, header included][u32 version = 1][u32 type = 8][u32 tag][plist]
//! ```

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

const HEADER_LEN: usize = 16;
const VERSION: u32 = 1;
/// Message type for XML plist payloads, the only kind modern usbmuxd uses.
const TYPE_PLIST: u32 = 8;
/// A reply larger than this is a corrupt length, not a real message. A
/// device list with dozens of devices is a few kilobytes.
const MAX_PAYLOAD: usize = 1 << 20;

/// Write one request with the given tag.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, tag: u32, body: &T) -> io::Result<()> {
    let mut payload = Vec::new();
    plist::to_writer_xml(&mut payload, body).map_err(io::Error::other)?;
    let total = u32::try_from(HEADER_LEN + payload.len())
        .map_err(|_| io::Error::other("usbmuxd request too large"))?;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    for field in [total, VERSION, TYPE_PLIST, tag] {
        frame.extend_from_slice(&field.to_le_bytes());
    }
    frame.extend_from_slice(&payload);
    w.write_all(&frame)
}

/// Read one message and decode its plist. Returns the tag with it.
pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<(u32, T)> {
    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header)?;
    let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let total = field(0) as usize;
    let tag = field(3);
    if !(HEADER_LEN..=HEADER_LEN + MAX_PAYLOAD).contains(&total) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("usbmuxd sent a message with an impossible length ({total} bytes)"),
        ));
    }
    let mut payload = vec![0u8; total - HEADER_LEN];
    r.read_exact(&mut payload)?;
    let body =
        plist::from_bytes(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((tag, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_message_round_trips_with_its_tag() {
        let body = BTreeMap::from([("MessageType".to_string(), "ListDevices".to_string())]);
        let mut buf = Vec::new();
        write_message(&mut buf, 7, &body).unwrap();

        assert_eq!(
            u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize,
            buf.len()
        );
        assert_eq!(u32::from_le_bytes(buf[4..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(buf[8..12].try_into().unwrap()), 8);

        let (tag, back): (u32, BTreeMap<String, String>) = read_message(&mut &buf[..]).unwrap();
        assert_eq!(tag, 7);
        assert_eq!(back, body);
    }

    #[test]
    fn an_impossible_length_is_rejected_without_allocating() {
        let mut buf = Vec::new();
        for field in [u32::MAX, 1, 8, 0] {
            buf.extend_from_slice(&field.to_le_bytes());
        }
        let err = read_message::<_, plist::Value>(&mut &buf[..]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_truncated_message_is_an_error() {
        let body = BTreeMap::from([("a".to_string(), "b".to_string())]);
        let mut buf = Vec::new();
        write_message(&mut buf, 1, &body).unwrap();
        buf.truncate(buf.len() - 3);
        assert!(read_message::<_, plist::Value>(&mut &buf[..]).is_err());
    }
}
