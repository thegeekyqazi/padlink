//! Client for usbmuxd, the daemon that multiplexes connections to iOS
//! devices over USB.
//!
//! Three things are needed from it (SPEC.md R1.1 to R1.3):
//!
//! * [`list_devices`]: which devices are plugged in right now.
//! * [`Listener`]: attach and detach events, which double as the hotplug
//!   source, so no udev rule is needed.
//! * [`connect`]: a TCP tunnel to a port on a device. After usbmuxd answers
//!   OK the Unix socket is a transparent byte pipe to that port.
//!
//! All I/O is blocking; callers run it on their own thread.
//!
//! The connection sequence follows `aaditya19saini/opendisplay-linux`
//! (`Linux/src/socket.cpp`, `discovery.cpp`) and libusbmuxd.

mod wire;

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where usbmuxd listens on Linux.
pub const DEFAULT_SOCKET: &str = "/var/run/usbmuxd";

const CLIENT_NAME: &str = "padlink";

/// A device as usbmuxd reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// usbmuxd's handle for this attachment. Changes on every replug.
    pub id: u32,
    /// The device's UDID. Stable across replugs.
    pub serial: String,
    pub transport: Transport,
    /// Link speed in bits per second, when usbmuxd reports it.
    pub speed_bps: Option<u64>,
}

impl Device {
    /// A short, stable label for output names and logs: the last six
    /// characters of the UDID, lower-cased.
    pub fn short_id(&self) -> String {
        let s: String = self
            .serial
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        s[s.len().saturating_sub(6)..].to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    /// The device is reachable over Wi-Fi sync, not a cable.
    Network,
}

/// A hotplug event from [`Listener`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Attached(Device),
    /// Carries the [`Device::id`] of the attachment that ended.
    Detached(u32),
}

/// Why a request to usbmuxd failed. Each message names the next step.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "usbmuxd is not reachable at {path}: {source}. \
         Start it with `sudo systemctl start usbmuxd`, or plug the iPad in (it starts on demand)"
    )]
    Unreachable { path: PathBuf, source: io::Error },

    #[error(
        "nothing is listening on port {port} of the iPad. \
         Open the OpenDisplay app and keep it in the foreground"
    )]
    Refused { port: u16 },

    #[error(
        "usbmuxd no longer knows this device. It was unplugged, or it is locked \
         or not trusted: unlock the iPad and tap Trust if it asks"
    )]
    DeviceGone,

    #[error("usbmuxd refused the request (result {0})")]
    Rejected(u64),

    #[error("usbmuxd sent an unexpected reply: {0}")]
    Protocol(String),

    #[error("lost the connection to usbmuxd: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

// usbmuxd result numbers.
const RESULT_OK: u64 = 0;
const RESULT_BAD_DEVICE: u64 = 2;
const RESULT_REFUSED: u64 = 3;

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Request<'a> {
    message_type: &'a str,
    client_version_string: &'a str,
    prog_name: &'a str,
    #[serde(rename = "DeviceID", skip_serializing_if = "Option::is_none")]
    device_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port_number: Option<u16>,
}

impl<'a> Request<'a> {
    fn new(message_type: &'a str) -> Self {
        Request {
            message_type,
            client_version_string: CLIENT_NAME,
            prog_name: CLIENT_NAME,
            device_id: None,
            port_number: None,
        }
    }
}

/// Any message usbmuxd sends: a result, a device list, or a hotplug event.
/// Unknown keys are ignored.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Reply {
    message_type: Option<String>,
    number: Option<u64>,
    #[serde(rename = "DeviceID")]
    device_id: Option<u32>,
    properties: Option<Properties>,
    device_list: Option<Vec<Reply>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Properties {
    connection_type: Option<String>,
    serial_number: Option<String>,
    connection_speed: Option<u64>,
}

impl Reply {
    fn into_device(self) -> Option<Device> {
        let id = self.device_id?;
        let p = self.properties?;
        Some(Device {
            id,
            serial: p.serial_number?,
            transport: match p.connection_type.as_deref() {
                Some("Network") => Transport::Network,
                _ => Transport::Usb,
            },
            speed_bps: p.connection_speed,
        })
    }

    fn result(&self) -> Result<u64> {
        self.number
            .ok_or_else(|| Error::Protocol("expected a Result message".into()))
    }
}

fn open(path: &Path) -> Result<UnixStream> {
    UnixStream::connect(path).map_err(|source| Error::Unreachable {
        path: path.to_path_buf(),
        source,
    })
}

fn exchange(sock: &mut UnixStream, request: &Request) -> Result<Reply> {
    wire::write_message(sock, 1, request)?;
    let (_tag, reply) = wire::read_message(sock)?;
    Ok(reply)
}

/// The devices attached right now.
pub fn list_devices(socket: &Path) -> Result<Vec<Device>> {
    let mut sock = open(socket)?;
    let reply = exchange(&mut sock, &Request::new("ListDevices"))?;
    let list = reply
        .device_list
        .ok_or_else(|| Error::Protocol("ListDevices reply has no DeviceList".into()))?;
    Ok(list.into_iter().filter_map(Reply::into_device).collect())
}

/// Open a tunnel to TCP `port` on the device. On success the returned socket
/// carries that connection's bytes and nothing else.
///
/// [`Error::Refused`] means nothing listens on the port (the receiver app is
/// not open); retry with backoff. [`Error::DeviceGone`] means the attachment
/// ended or the device is not usable; wait for the next attach event.
pub fn connect(socket: &Path, device_id: u32, port: u16) -> Result<UnixStream> {
    let mut sock = open(socket)?;
    let mut request = Request::new("Connect");
    request.device_id = Some(device_id);
    // usbmuxd reads the port as a 16-bit value in network byte order stored
    // in a host integer, so 9000 (0x2328) goes on the wire as 0x2823.
    request.port_number = Some(port.to_be());
    match exchange(&mut sock, &request)?.result()? {
        RESULT_OK => Ok(sock),
        RESULT_REFUSED => Err(Error::Refused { port }),
        RESULT_BAD_DEVICE => Err(Error::DeviceGone),
        n => Err(Error::Rejected(n)),
    }
}

/// A subscription to attach and detach events.
///
/// usbmuxd first replays an `Attached` event for every device already
/// present, so a fresh listener needs no separate [`list_devices`] call.
pub struct Listener {
    sock: UnixStream,
}

impl Listener {
    pub fn new(socket: &Path) -> Result<Self> {
        let mut sock = open(socket)?;
        match exchange(&mut sock, &Request::new("Listen"))?.result()? {
            RESULT_OK => Ok(Listener { sock }),
            n => Err(Error::Rejected(n)),
        }
    }

    /// Block until the next event. With a timeout set, returns `Ok(None)`
    /// when it elapses, so the caller can check for shutdown.
    pub fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        self.sock.set_read_timeout(timeout)?;
        loop {
            // Wait for the first byte separately: a timeout here means "no
            // event", while a timeout inside a message would desynchronise
            // the stream.
            let mut first = [0u8; 1];
            match (&self.sock).read(&mut first) {
                Ok(0) => {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "usbmuxd closed the event stream",
                    )));
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
            self.sock.set_read_timeout(None)?;
            let (_tag, reply): (u32, Reply) =
                wire::read_message(&mut (&first[..]).chain(&self.sock))?;
            self.sock.set_read_timeout(timeout)?;

            match reply.message_type.as_deref() {
                Some("Attached") => {
                    if let Some(device) = reply.into_device() {
                        return Ok(Some(Event::Attached(device)));
                    }
                }
                Some("Detached") => {
                    if let Some(id) = reply.device_id {
                        return Ok(Some(Event::Detached(id)));
                    }
                }
                // "Paired" and anything newer: not needed.
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(serial: &str) -> Device {
        Device {
            id: 1,
            serial: serial.into(),
            transport: Transport::Usb,
            speed_bps: None,
        }
    }

    #[test]
    fn short_id_is_the_udid_tail_lowercased() {
        assert_eq!(device("00008101-000479A434BA601E").short_id(), "ba601e");
        assert_eq!(device("ab-c").short_id(), "abc");
        assert_eq!(device("").short_id(), "");
    }

    #[test]
    fn errors_name_the_next_step() {
        let refused = Error::Refused { port: 9000 }.to_string();
        assert!(refused.contains("Open the OpenDisplay app"), "{refused}");
        let gone = Error::DeviceGone.to_string();
        assert!(gone.contains("unlock") && gone.contains("Trust"), "{gone}");
    }
}
