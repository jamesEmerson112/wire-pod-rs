//! Spike S3: prove the audio stack (libopus build, ogg framing, WebRTC VAD)
//! works on this platform, and run the Go-parity filter chain over the real
//! warm-up sample, printing goldens for later engine-parity diffing.

mod dsp;

use sha2::{Digest, Sha256};
use webrtc_vad::{SampleRate, Vad, VadMode};

fn main() {
    opus_roundtrip();
    ogg_roundtrip();
    vad_and_filter();
    println!("S3 PASS");
}

fn synth_sine() -> Vec<i16> {
    // 2 s of 440 Hz at 16 kHz mono.
    (0..32000)
        .map(|i| {
            let t = i as f64 / 16000.0;
            ((t * 440.0 * 2.0 * std::f64::consts::PI).sin() * 8000.0) as i16
        })
        .collect()
}

fn opus_roundtrip() {
    let pcm = synth_sine();
    let mut enc =
        opus::Encoder::new(16000, opus::Channels::Mono, opus::Application::Voip).expect("encoder");
    let mut dec = opus::Decoder::new(16000, opus::Channels::Mono).expect("decoder");
    let mut decoded_total = 0usize;
    let mut energy = 0f64;
    // 20 ms frames = 320 samples.
    for frame in pcm.chunks_exact(320) {
        let packet = enc.encode_vec(frame, 4000).expect("encode");
        let mut out = vec![0i16; 320];
        let n = dec.decode(&packet, &mut out, false).expect("decode");
        decoded_total += n;
        energy += out.iter().map(|&s| f64::from(s).abs()).sum::<f64>();
    }
    assert_eq!(decoded_total, 32000, "decoded sample count");
    assert!(energy > 1_000_000.0, "decoded audio has energy");
    println!("opus roundtrip: {decoded_total} samples, ok");
}

fn ogg_roundtrip() {
    let pcm = synth_sine();
    let mut enc =
        opus::Encoder::new(16000, opus::Channels::Mono, opus::Application::Voip).expect("encoder");
    let packets: Vec<Vec<u8>> = pcm
        .chunks_exact(320)
        .map(|f| enc.encode_vec(f, 4000).expect("encode"))
        .collect();

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut w = ogg::PacketWriter::new(&mut buf);
        let n = packets.len();
        for (i, p) in packets.iter().enumerate() {
            let end = if i == n - 1 {
                ogg::PacketWriteEndInfo::EndStream
            } else {
                ogg::PacketWriteEndInfo::NormalPacket
            };
            w.write_packet(p.clone(), 0xf00d, end, (i as u64 + 1) * 320)
                .expect("write packet");
        }
    }
    let mut r = ogg::PacketReader::new(std::io::Cursor::new(&buf));
    let mut count = 0usize;
    while let Some(p) = r.read_packet().expect("read packet") {
        assert_eq!(p.data, packets[count], "packet differs");
        count += 1;
    }
    assert_eq!(count, packets.len(), "packet count");
    println!(
        "ogg roundtrip: {count} packets, {} container bytes, ok",
        buf.len()
    );
}

fn vad_and_filter() {
    let pcm =
        std::fs::read("C:/Program Files/wire-pod/chipper/stttest.pcm").expect("read stttest.pcm");
    // Filter in 4096-byte chunks, state reset per chunk (Go behavior).
    let mut filtered: Vec<u8> = Vec::with_capacity(pcm.len());
    for chunk in pcm.chunks(4096) {
        filtered.extend(dsp::high_pass_chunk(chunk));
    }
    let mut h = Sha256::new();
    h.update(&filtered);
    let digest = format!("{:x}", h.finalize());

    let mut vad = Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive);
    let samples: Vec<i16> = filtered
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let mut active = 0usize;
    let mut total = 0usize;
    for frame in samples.chunks_exact(160) {
        total += 1;
        if vad.is_voice_segment(frame).expect("vad") {
            active += 1;
        }
    }
    assert!(
        active > 0,
        "no active speech frames detected in stttest.pcm"
    );
    println!(
        "vad+filter: {total} frames, {active} active, {} inactive",
        total - active
    );
    println!("filtered-pcm sha256 (golden, 4096B chunks): {digest}");
}
