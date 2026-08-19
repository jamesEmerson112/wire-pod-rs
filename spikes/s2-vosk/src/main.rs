//! Spike S2: prove the Rust `vosk` crate drives libvosk on Windows/MSVC,
//! using the same model the running Go server uses, transcribing the same
//! warm-up sample (`stttest.pcm`, raw s16le mono 16 kHz).

const MODEL_PATH: &str = "C:/Users/voan2/AppData/Roaming/wire-pod/vosk/models/en-US/model";
const PCM_PATH: &str = "C:/Program Files/wire-pod/chipper/stttest.pcm";

fn main() {
    let pcm = std::fs::read(PCM_PATH).expect("read stttest.pcm");
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    let model = vosk::Model::new(MODEL_PATH).expect("load vosk model (read-only)");
    let mut rec = vosk::Recognizer::new(&model, 16000.0).expect("create recognizer");

    for chunk in samples.chunks(2048) {
        rec.accept_waveform(chunk).expect("accept_waveform");
    }
    let result = rec.final_result();
    let text = result
        .single()
        .map(|r| r.text.to_string())
        .unwrap_or_default();
    assert!(!text.trim().is_empty(), "empty transcript");
    println!("transcript: {text}");
    println!("S2 PASS");
}
