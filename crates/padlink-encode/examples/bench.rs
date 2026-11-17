//! Time the encoder alone: upload, colour conversion and encode of a
//! 2360x1640 frame, per backend. One number per stage of SPEC.md section 9
//! that this crate owns.
//!
//!     cargo run --release -p padlink-encode --example bench

use std::time::Instant;

use padlink_encode::{Backend, Encoder, Settings};

fn main() {
    // An optional WIDTHxHEIGHT argument; the iPad panel by default.
    let (width, height) = std::env::args()
        .skip(1)
        .find_map(|a| {
            let (w, h) = a.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((2360u32, 1640u32));
    println!("{width}x{height}");
    let stride = width as usize * 4;
    let mut frame = vec![0u8; stride * height as usize];
    for (i, px) in frame.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i % width as usize, i / width as usize);
        px.copy_from_slice(&[(x / 8) as u8, (y / 8) as u8, ((x + y) / 16) as u8, 0]);
    }

    // Extra codec options as key=value arguments, to try settings out.
    let options: Vec<(String, String)> = std::env::args()
        .skip(1)
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();

    for backend in [Backend::Vaapi, Backend::X264, Backend::OpenH264] {
        let settings = Settings {
            width,
            height,
            fps: 60,
            bitrate_bps: 25_000_000,
            backend,
            render_node: "/dev/dri/renderD128".into(),
            codec_options: options.clone(),
        };
        let mut encoder = match Encoder::open(&settings) {
            Ok(opened) => opened.encoder,
            Err(e) => {
                println!("{backend:?}: not available: {e}");
                continue;
            }
        };
        let mut times = Vec::new();
        let (mut prepare, mut encode) = (Vec::new(), Vec::new());
        let mut bytes = 0usize;
        for n in 0..240usize {
            // Scroll a band of noise-like rows, as a terminal printing text
            // would. BAND=8 in the environment makes it a thin strip, close
            // to a pointer moving.
            let band = std::env::var("BAND")
                .ok()
                .and_then(|b| b.parse().ok())
                .unwrap_or(200usize);
            let top = (n * 37) % (height as usize - band);
            for row in frame[top * stride..(top + band) * stride].chunks_exact_mut(stride) {
                row.iter_mut()
                    .enumerate()
                    .for_each(|(i, b)| *b = (i * 31 + n * 17) as u8);
            }
            let start = Instant::now();
            let packet = encoder.encode_bgrx(&frame, stride, false).unwrap().unwrap();
            if n >= 30 {
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                prepare.push(encoder.timings().prepare_ms);
                encode.push(encoder.timings().encode_ms);
                bytes += packet.data.len();
            }
        }
        times.sort_by(f64::total_cmp);
        let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        println!(
            "{:<18} p50 {:5.1} ms  p95 {:5.1} ms  max {:5.1} ms  (prepare {:4.1} + encode {:4.1})  {:5.1} Mbit/s at 60 fps",
            encoder.kind().label(),
            pct(0.5),
            pct(0.95),
            pct(1.0),
            median(&mut prepare),
            median(&mut encode),
            bytes as f64 * 8.0 / times.len() as f64 * 60.0 / 1e6
        );
    }
}
