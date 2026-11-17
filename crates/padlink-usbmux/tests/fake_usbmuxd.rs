//! The client against a fake usbmuxd socket: attach, detach, connect and
//! connect-refused (SPEC.md section 9; R1.1, R1.2, R1.3).
//!
//! The fake frames messages itself instead of reusing the crate's code, so
//! a framing mistake cannot hide on both sides.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use padlink_usbmux::{Device, Error, Event, Listener, Transport, connect, list_devices};
use plist::{Dictionary, Value};

const SERIAL: &str = "00008101-000479A434BA601E";

/// What the fake does when a client asks to `Connect`.
#[derive(Clone, Copy)]
enum OnConnect {
    /// Answer OK, then echo bytes back with a prefix, like a device port.
    Accept,
    /// Answer with this usbmuxd result number.
    Fail(u64),
}

struct Fake {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fake {
    fn start(on_connect: OnConnect) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usbmuxd");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                thread::spawn(move || serve(stream, on_connect));
            }
        });
        Fake { path, _dir: dir }
    }
}

fn read_request(s: &mut UnixStream) -> Option<Dictionary> {
    let mut header = [0u8; 16];
    s.read_exact(&mut header).ok()?;
    let total = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; total - 16];
    s.read_exact(&mut payload).ok()?;
    plist::from_bytes::<Value>(&payload).ok()?.into_dictionary()
}

fn send(s: &mut UnixStream, body: Dictionary) {
    let mut payload = Vec::new();
    Value::Dictionary(body).to_writer_xml(&mut payload).unwrap();
    let mut frame = Vec::new();
    for field in [16 + payload.len() as u32, 1, 8, 1] {
        frame.extend_from_slice(&field.to_le_bytes());
    }
    frame.extend_from_slice(&payload);
    s.write_all(&frame).unwrap();
}

fn dict<const N: usize>(entries: [(&str, Value); N]) -> Dictionary {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn result(number: u64) -> Dictionary {
    dict([("MessageType", "Result".into()), ("Number", number.into())])
}

fn attached(id: u64) -> Dictionary {
    dict([
        ("MessageType", "Attached".into()),
        ("DeviceID", id.into()),
        (
            "Properties",
            Value::Dictionary(dict([
                ("ConnectionType", "USB".into()),
                ("ConnectionSpeed", 480_000_000u64.into()),
                ("DeviceID", id.into()),
                ("SerialNumber", SERIAL.into()),
                // A key the client does not know about must be ignored.
                ("LocationID", 0u64.into()),
            ])),
        ),
    ])
}

fn serve(mut s: UnixStream, on_connect: OnConnect) {
    let Some(request) = read_request(&mut s) else {
        return;
    };
    match request.get("MessageType").and_then(Value::as_string) {
        Some("ListDevices") => send(
            &mut s,
            dict([(
                "DeviceList",
                Value::Array(vec![Value::Dictionary(attached(4))]),
            )]),
        ),
        Some("Listen") => {
            send(&mut s, result(0));
            send(&mut s, attached(4));
            // An event type the client has no use for.
            send(
                &mut s,
                dict([("MessageType", "Paired".into()), ("DeviceID", 4u64.into())]),
            );
            send(
                &mut s,
                dict([
                    ("MessageType", "Detached".into()),
                    ("DeviceID", 4u64.into()),
                ]),
            );
            // Then stay quiet, like a real usbmuxd with nothing to report.
            thread::sleep(Duration::from_secs(5));
        }
        Some("Connect") => {
            // Port 9000 must arrive byte-swapped: 0x2328 -> 0x2823.
            let port = request
                .get("PortNumber")
                .and_then(Value::as_unsigned_integer);
            let device = request.get("DeviceID").and_then(Value::as_unsigned_integer);
            if port != Some(0x2823) || device != Some(4) {
                send(&mut s, result(1));
                return;
            }
            match on_connect {
                OnConnect::Fail(n) => send(&mut s, result(n)),
                OnConnect::Accept => {
                    send(&mut s, result(0));
                    let mut buf = [0u8; 64];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        s.write_all(b"dev:").unwrap();
                        s.write_all(&buf[..n]).unwrap();
                    }
                }
            }
        }
        _ => send(&mut s, result(1)),
    }
}

fn ipad() -> Device {
    Device {
        id: 4,
        serial: SERIAL.into(),
        transport: Transport::Usb,
        speed_bps: Some(480_000_000),
    }
}

#[test]
fn lists_attached_devices() {
    let fake = Fake::start(OnConnect::Accept);
    assert_eq!(list_devices(&fake.path).unwrap(), vec![ipad()]);
}

#[test]
fn connect_becomes_a_byte_pipe_to_the_device_port() {
    let fake = Fake::start(OnConnect::Accept);
    let mut tunnel = connect(&fake.path, 4, 9000).unwrap();
    tunnel.write_all(b"hello").unwrap();
    let mut back = [0u8; 9];
    tunnel.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"dev:hello");
}

#[test]
fn a_closed_receiver_app_is_reported_as_refused() {
    let fake = Fake::start(OnConnect::Fail(3));
    match connect(&fake.path, 4, 9000) {
        Err(Error::Refused { port: 9000 }) => {}
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn an_unplugged_or_untrusted_device_is_reported_as_gone() {
    let fake = Fake::start(OnConnect::Fail(2));
    assert!(matches!(
        connect(&fake.path, 4, 9000),
        Err(Error::DeviceGone)
    ));
}

#[test]
fn other_failures_keep_their_result_number() {
    let fake = Fake::start(OnConnect::Fail(6));
    assert!(matches!(
        connect(&fake.path, 4, 9000),
        Err(Error::Rejected(6))
    ));
}

#[test]
fn listener_reports_attach_then_detach_and_skips_unknown_events() {
    let fake = Fake::start(OnConnect::Accept);
    let mut listener = Listener::new(&fake.path).unwrap();
    let wait = Some(Duration::from_secs(2));
    assert_eq!(
        listener.next_event(wait).unwrap(),
        Some(Event::Attached(ipad()))
    );
    assert_eq!(listener.next_event(wait).unwrap(), Some(Event::Detached(4)));
}

#[test]
fn listener_times_out_quietly_when_nothing_happens() {
    let fake = Fake::start(OnConnect::Accept);
    let mut listener = Listener::new(&fake.path).unwrap();
    let wait = Some(Duration::from_secs(2));
    listener.next_event(wait).unwrap();
    listener.next_event(wait).unwrap();
    assert_eq!(
        listener
            .next_event(Some(Duration::from_millis(50)))
            .unwrap(),
        None
    );
}

#[test]
fn a_missing_socket_says_how_to_start_usbmuxd() {
    let dir = tempfile::tempdir().unwrap();
    let err = list_devices(&dir.path().join("absent")).unwrap_err();
    assert!(matches!(err, Error::Unreachable { .. }));
    assert!(err.to_string().contains("systemctl start usbmuxd"), "{err}");
}
