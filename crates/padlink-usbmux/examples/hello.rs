//! List devices, tunnel to port 9000 on the first USB one and print the
//! receiver's first message. Read-only: sends nothing to the receiver.
//!
//!     cargo run -p padlink-usbmux --example hello

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use padlink_usbmux::{DEFAULT_SOCKET, Transport, connect, list_devices};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = Path::new(DEFAULT_SOCKET);
    let devices = list_devices(socket)?;
    for d in &devices {
        println!(
            "device {} ({:?}, …{}) {:?} bit/s",
            d.id,
            d.transport,
            d.short_id(),
            d.speed_bps
        );
    }
    let Some(device) = devices.iter().find(|d| d.transport == Transport::Usb) else {
        return Err("no USB device: plug the iPad in and unlock it".into());
    };

    let start = Instant::now();
    let mut tunnel = connect(socket, device.id, 9000)?;
    tunnel.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut len = [0u8; 4];
    tunnel.read_exact(&mut len)?;
    let mut payload = vec![0u8; u32::from_be_bytes(len) as usize];
    tunnel.read_exact(&mut payload)?;
    println!(
        "first message after {:?}, {} bytes:",
        start.elapsed(),
        payload.len()
    );
    println!("{}", String::from_utf8_lossy(&payload));
    Ok(())
}
