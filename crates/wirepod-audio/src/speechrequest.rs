//! Translation of `pkg/wirepod/speechrequest/speechrequest.go`.

use std::f64::consts::PI;

use async_trait::async_trait;
use webrtc_vad::{SampleRate, Vad, VadMode};
use wirepod_core::{ConnError, StatusCode};

/// Go keeps the gRPC stream in a `Stream interface{}` and type-switches it in
/// six places. The three tonic stream types are distinct, so each gets a small
/// adapter and the request reads through this instead.
#[async_trait]
pub trait ChunkSource: Send {
    /// One `stream.Recv()`: the next audio chunk, or `None` at end of stream.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ConnError>;
}

/// grpc-go's `Recv` answers `io.EOF` once the robot stops sending, and Go logs
/// and returns that like any other error.
fn eof() -> ConnError {
    ConnError::new(StatusCode::Unknown, "EOF")
}

/// Go's `*webrtcvad.VAD`. The crate does not declare its handle `Send`, though
/// `fvad` owns its instance outright and a request is only ever driven by one
/// task at a time.
pub struct VadInst(Vad);

// SAFETY: the pointer inside is an owned `fvad` instance with no shared state.
unsafe impl Send for VadInst {}

impl Default for VadInst {
    fn default() -> Self {
        Self(Vad::new_with_rate_and_mode(
            SampleRate::Rate16kHz,
            VadMode::Aggressive,
        ))
    }
}

/// Which chipper stream a request arrived on. Go tells the three apart by
/// type-switching `req interface{}` inside [`req_to_speech_request`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestKind {
    /// `*vtt.IntentRequest`.
    Intent,
    /// `*vtt.KnowledgeGraphRequest`.
    KnowledgeGraph,
    /// `*vtt.IntentGraphRequest`.
    IntentGraph,
}

/// The five fields [`req_to_speech_request`] reads off a `vtt` request.
pub struct RequestParts {
    pub kind: RequestKind,
    pub device: String,
    pub session: String,
    pub stream: Box<dyn ChunkSource>,
    pub first_req: Vec<u8>,
}

#[derive(Default)]
pub struct SpeechRequest {
    pub device: String,
    pub session: String,
    pub first_req: Vec<u8>,
    pub stream: Option<Box<dyn ChunkSource>>,
    pub is_kg: bool,
    pub is_ig: bool,
    pub mic_data: Vec<u8>,
    pub decoded_mic_data: Vec<u8>,
    pub filtered_mic_data: Vec<u8>,
    pub prev_len: usize,
    pub prev_len_raw: usize,
    pub inactive_frames: u32,
    pub active_frames: u32,
    pub vad_inst: VadInst,
    pub last_audio_chunk: Vec<u8>,
    pub is_opus: bool,
    pub opus_stream: Option<OggStream>,
}

pub fn bytes_to_samples(buf: &[u8]) -> Vec<i16> {
    let mut samples = vec![0i16; buf.len() / 2];
    for (i, sample) in samples.iter_mut().enumerate() {
        *sample = i16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]);
    }
    samples
}

impl SpeechRequest {
    pub fn opus_detect(&self) -> bool {
        let mut is_opus = false;
        if !self.first_req.is_empty() {
            if self.first_req[0] == 0x4f {
                tracing::debug!(target: "voice", bot = %self.device, "stream type: OPUS");
                is_opus = true;
            } else {
                is_opus = false;
                tracing::debug!(target: "voice", bot = %self.device, "stream type: PCM");
            }
        }
        is_opus
    }

    pub fn opus_decode(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.is_opus {
            // Go dereferences a nil OpusStream here if one was never built.
            let Some(stream) = self.opus_stream.as_mut() else {
                tracing::error!(target: "voice", bot = %self.device, "no opus stream");
                return Vec::new();
            };
            match stream.decode(chunk) {
                Ok(n) => n,
                Err(err) => {
                    tracing::error!(target: "voice", bot = %self.device, "{err}");
                    Vec::new()
                }
            }
        } else {
            chunk.to_vec()
        }
    }
}

pub fn split_vad(buf: &[u8]) -> Vec<&[u8]> {
    let mut chunk = Vec::new();
    let mut buf = buf;
    while buf.len() >= 320 {
        chunk.push(&buf[..320]);
        buf = &buf[320..];
    }
    chunk
}

pub fn bytes_to_int_vad(
    stream: &mut OggStream,
    data: &[u8],
    die: bool,
    is_opus: bool,
) -> Vec<Vec<u8>> {
    // detect if data is pcm or opus
    if die {
        return Vec::new();
    }
    if is_opus {
        // opus
        let n = match stream.decode(data) {
            Ok(n) => n,
            Err(err) => {
                tracing::error!(target: "voice", "{err}");
                Vec::new()
            }
        };
        split_vad(&n).into_iter().map(<[u8]>::to_vec).collect()
    } else {
        // pcm
        split_vad(data).into_iter().map(<[u8]>::to_vec).collect()
    }
}

impl SpeechRequest {
    /// Uses VAD to detect when the user stops speaking
    pub fn detect_end_of_speech(&mut self) -> (bool, bool) {
        // changes inactive_frames and active_frames in self
        let inactive_num_max = 23;
        for chunk in split_vad(&self.last_audio_chunk) {
            let frame = bytes_to_samples(chunk);
            let active = match self.vad_inst.0.is_voice_segment(&frame) {
                Ok(active) => active,
                Err(()) => {
                    tracing::error!(target: "voice", bot = %self.device, "VAD: invalid frame length");
                    return (true, false);
                }
            };
            if active {
                self.active_frames += 1;
                self.inactive_frames = 0;
            } else {
                self.inactive_frames += 1;
            }
            if self.inactive_frames >= inactive_num_max && self.active_frames > 18 {
                tracing::debug!(target: "voice", bot = %self.device, "end of speech detected");
                return (true, true);
            }
        }
        if self.active_frames < 5 {
            return (false, false);
        }
        (false, true)
    }
}

/// `None` is Go's `binary.Read` error, which a trailing odd byte produces.
fn bytes_to_int16(data: &[u8]) -> Option<Vec<i16>> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    Some(bytes_to_samples(data))
}

fn int16_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn apply_gain(samples: &mut [i16], gain: f64) {
    for sample in samples.iter_mut() {
        let amplified_sample = f64::from(*sample) * gain;
        *sample = if amplified_sample > f64::from(i16::MAX) {
            i16::MAX
        } else if amplified_sample < f64::from(i16::MIN) {
            i16::MIN
        } else {
            amplified_sample as i16
        };
    }
}

/// remove noise
fn high_pass_filter(data: &[u8]) -> Vec<u8> {
    let sample_rate = 16000;
    let cutoff_freq = 300.0;
    let Some(mut samples) = bytes_to_int16(data) else {
        return Vec::new();
    };
    apply_gain(&mut samples, 5.0);
    if samples.is_empty() {
        // Go reads samples[0] below with no length check and panics here.
        tracing::error!(target: "voice", "highpass filter: empty chunk");
        return Vec::new();
    }
    let mut filtered_samples = vec![0.0f64; samples.len()];
    let rc = 1.0 / (2.0 * PI * cutoff_freq);
    let dt = 1.0 / f64::from(sample_rate);
    // Go's quirk: a low-pass alpha inside a high-pass recurrence. Kept.
    let alpha = dt / (rc + dt);

    let mut previous = f64::from(samples[0]);
    for i in 1..samples.len() {
        let current = f64::from(samples[i]);
        let filtered = alpha * (filtered_samples[i - 1] + current - previous);
        filtered_samples[i] = filtered;
        previous = current;
    }
    // Go's int16(float64) truncates through the int pipeline rather than
    // saturating, which `as i64 as i16` reproduces at any realistic magnitude.
    let mut int16_filtered_samples: Vec<i16> =
        filtered_samples.iter().map(|&s| s as i64 as i16).collect();

    apply_gain(&mut int16_filtered_samples, 1.5);
    int16_to_bytes(&int16_filtered_samples)
}

/// Converts a vtt request to a SpeechRequest, which allows functions like
/// detect_end_of_speech to work
pub fn req_to_speech_request(req: RequestParts) -> SpeechRequest {
    // Go builds the VAD here and calls SetMode(2) before checking the error from
    // webrtcvad.New(); the Rust constructor cannot fail, so the default holds
    // the same 16 kHz mode-2 instance and there is no error to check.
    let mut request = SpeechRequest {
        prev_len: 0,
        ..SpeechRequest::default()
    };
    match req.kind {
        RequestKind::Intent => {}
        RequestKind::KnowledgeGraph => request.is_kg = true,
        RequestKind::IntentGraph => request.is_ig = true,
    }
    request.device = req.device;
    request.session = req.session;
    request.stream = Some(req.stream);
    request.first_req = req.first_req;
    request.mic_data.extend_from_slice(&request.first_req);
    let is_opus = request.opus_detect();
    if is_opus {
        let mut opus_stream = OggStream::default();
        let decoded_first_req = opus_stream.decode(&request.first_req).unwrap_or_default();
        request.opus_stream = Some(opus_stream);
        request.first_req = high_pass_filter(&decoded_first_req);
        request
            .filtered_mic_data
            .extend_from_slice(&request.first_req);
        request
            .decoded_mic_data
            .extend_from_slice(&decoded_first_req);
        request.last_audio_chunk = request.filtered_mic_data[request.prev_len..].to_vec();
        request.prev_len = request.decoded_mic_data.len();
        request.is_opus = true;
    }
    request
}

impl SpeechRequest {
    /// Returns the next chunk in the stream as 16000 Hz PCM
    pub async fn get_next_stream_chunk(&mut self) -> Result<Vec<u8>, ConnError> {
        // returns next chunk in voice stream as pcm
        let chunk = self.recv().await?;
        self.mic_data.extend_from_slice(&chunk);
        let decoded = self.opus_decode(&chunk);
        self.decoded_mic_data.extend_from_slice(&decoded);
        // Go decodes the same chunk a second time here, which advances the Opus
        // decoder again, so the filtered copy is not the filter of the decoded
        // copy. Kept.
        let filtered = high_pass_filter(&self.opus_decode(&chunk));
        self.filtered_mic_data.extend_from_slice(&filtered);
        let data_return = self.decoded_mic_data[self.prev_len..].to_vec();
        self.last_audio_chunk = self.filtered_mic_data[self.prev_len..].to_vec();
        self.prev_len = self.decoded_mic_data.len();
        Ok(data_return)
    }

    /// Returns next chunk in the stream as whatever the original format is
    /// (OPUS 99% of the time)
    pub async fn get_next_stream_chunk_opus(&mut self) -> Result<Vec<u8>, ConnError> {
        let chunk = self.recv().await?;
        self.mic_data.extend_from_slice(&chunk);
        let decoded = self.opus_decode(&chunk);
        self.decoded_mic_data.extend_from_slice(&decoded);
        let data_return = self.mic_data[self.prev_len_raw..].to_vec();
        self.last_audio_chunk = self.decoded_mic_data[self.prev_len..].to_vec();
        self.prev_len = self.decoded_mic_data.len();
        self.prev_len_raw = self.mic_data.len();
        Ok(data_return)
    }

    /// The `stream.Recv()` both getters open with, plus Go's final
    /// `"invalid type"` arm for a stream that is none of the three.
    async fn recv(&mut self) -> Result<Vec<u8>, ConnError> {
        let Some(stream) = self.stream.as_mut() else {
            tracing::error!(target: "voice", bot = %self.device, "invalid type");
            return Err(ConnError::new(StatusCode::Unknown, "invalid type"));
        };
        match stream.next_chunk().await {
            Ok(Some(chunk)) => Ok(chunk),
            Ok(None) => {
                let chunk_err = eof();
                tracing::error!(target: "voice", bot = %self.device, "{}", chunk_err.desc);
                Err(chunk_err)
            }
            Err(chunk_err) => {
                tracing::error!(target: "voice", bot = %self.device, "{}", chunk_err.desc);
                Err(chunk_err)
            }
        }
    }
}

/// The decode half of `github.com/digital-dream-labs/opus-go`'s `OggStream`,
/// which is a library the Go server depends on rather than a file of it. Go
/// feeds libogg's sync state and reads the channel count and the input sample
/// rate out of the `OpusHead` packet; this walks whole pages out of an
/// accumulating buffer instead and does the same with the packets it finds.
#[derive(Default)]
pub struct OggStream {
    buf: Vec<u8>,
    partial: Vec<u8>,
    headers: usize,
    decoder: Option<opus::Decoder>,
}

impl OggStream {
    pub fn decode(&mut self, ogg_bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.buf.extend_from_slice(ogg_bytes);
        let buf = std::mem::take(&mut self.buf);
        let mut pos = 0usize;
        let result = self.decode_pages(&buf, &mut pos);
        self.buf = buf[pos..].to_vec();
        result
    }

    fn decode_pages(&mut self, buf: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
        let mut to_return: Vec<u8> = Vec::new();
        while buf.len() - *pos >= 27 {
            if &buf[*pos..*pos + 4] != b"OggS" {
                *pos += 1;
                continue;
            }
            let segments = buf[*pos + 26] as usize;
            let header = 27 + segments;
            if buf.len() - *pos < header {
                break;
            }
            let table = &buf[*pos + 27..*pos + header];
            let body_len: usize = table.iter().map(|&n| usize::from(n)).sum();
            if buf.len() - *pos < header + body_len {
                break;
            }
            let mut at = *pos + header;
            for &lacing in table {
                let lacing = usize::from(lacing);
                self.partial.extend_from_slice(&buf[at..at + lacing]);
                at += lacing;
                if lacing == 255 {
                    continue;
                }
                let packet = std::mem::take(&mut self.partial);
                if self.headers < 2 {
                    if self.headers == 0 {
                        self.open_decoder(&packet)?;
                    }
                    self.headers += 1;
                    continue;
                }
                let decoder = self
                    .decoder
                    .as_mut()
                    .ok_or_else(|| "no opus decoder".to_string())?;
                let mut sample_buf = vec![0i16; 4096];
                let n_decoded = decoder
                    .decode(&packet, &mut sample_buf, false)
                    .map_err(|err| err.to_string())?;
                to_return.extend(sample_buf[..n_decoded].iter().flat_map(|s| s.to_le_bytes()));
            }
            *pos += header + body_len;
        }
        Ok(to_return)
    }

    /// Go's `ReadInfoFromHeaders`: channel count at byte 9 of `OpusHead`, input
    /// sample rate in the four little-endian bytes at 12.
    fn open_decoder(&mut self, packet: &[u8]) -> Result<(), String> {
        if packet.len() < 16 {
            return Err("Error reading packet from ogg header page 1".to_string());
        }
        let channels = match packet[9] {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            other => return Err(format!("unsupported channel count {other}")),
        };
        let sample_rate = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
        self.decoder =
            Some(opus::Decoder::new(sample_rate, channels).map_err(|err| err.to_string())?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent_frames(count: usize) -> Vec<u8> {
        vec![0u8; count * 320]
    }

    #[test]
    fn speech_ends_after_23_quiet_frames_once_18_are_active() {
        // Silence is the one verdict the VAD is certain about, so the active
        // side is set directly and only the counter rule is under test.
        let mut req = SpeechRequest {
            active_frames: 19,
            last_audio_chunk: silent_frames(22),
            ..SpeechRequest::default()
        };
        assert_eq!(req.detect_end_of_speech(), (false, true));
        assert_eq!(req.inactive_frames, 22);

        req.last_audio_chunk = silent_frames(1);
        assert_eq!(req.detect_end_of_speech(), (true, true));
        assert_eq!(req.inactive_frames, 23);

        // Too few active frames is Go's "not worth processing" answer, and the
        // trailing 200 bytes are dropped rather than made into a short frame.
        let mut quiet = SpeechRequest {
            last_audio_chunk: silent_frames(30),
            ..SpeechRequest::default()
        };
        quiet.last_audio_chunk.extend_from_slice(&[0u8; 200]);
        assert_eq!(quiet.detect_end_of_speech(), (false, false));
        assert_eq!(quiet.inactive_frames, 30);
    }

    struct NoChunks;

    #[async_trait]
    impl ChunkSource for NoChunks {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ConnError> {
            Ok(None)
        }
    }

    /// One second of 440 Hz in an Ogg-Opus container, the shape the robot sends.
    fn ogg_opus(samples: usize) -> Vec<u8> {
        let mut enc =
            opus::Encoder::new(16000, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let pcm: Vec<i16> = (0..samples)
            .map(|i| ((i as f64 / 16000.0 * 440.0 * 2.0 * PI).sin() * 8000.0) as i16)
            .collect();
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]); // version, channels
        head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&16000u32.to_le_bytes()); // input sample rate
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family

        let mut buf: Vec<u8> = Vec::new();
        let mut writer = ogg::PacketWriter::new(&mut buf);
        writer
            .write_packet(head, 1, ogg::PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        writer
            .write_packet(
                b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
                1,
                ogg::PacketWriteEndInfo::EndPage,
                0,
            )
            .unwrap();
        let frames: Vec<Vec<u8>> = pcm
            .chunks_exact(320)
            .map(|f| enc.encode_vec(f, 4000).unwrap())
            .collect();
        for (i, frame) in frames.iter().enumerate() {
            let end = if i == frames.len() - 1 {
                ogg::PacketWriteEndInfo::EndStream
            } else {
                ogg::PacketWriteEndInfo::NormalPacket
            };
            writer
                .write_packet(frame.clone(), 1, end, (i as u64 + 1) * 960)
                .unwrap();
        }
        drop(writer);
        buf
    }

    #[test]
    fn an_opus_first_request_is_sniffed_decoded_and_filtered() {
        let container = ogg_opus(3200);
        assert_eq!(container[0], 0x4f);
        let req = req_to_speech_request(RequestParts {
            kind: RequestKind::IntentGraph,
            device: "00303f28".to_string(),
            session: "s".to_string(),
            stream: Box::new(NoChunks),
            first_req: container.clone(),
        });
        assert!(req.is_ig && req.is_opus);
        assert_eq!(req.mic_data, container);
        assert_eq!(req.decoded_mic_data.len(), 3200 * 2);
        assert_eq!(req.first_req, req.filtered_mic_data);
        assert_eq!(req.last_audio_chunk, req.filtered_mic_data);
        assert_eq!(req.prev_len, req.decoded_mic_data.len());

        // A first byte that is not 0x4f stays raw PCM, and an empty one says PCM.
        let pcm = SpeechRequest {
            first_req: vec![0x00, 0x01],
            ..SpeechRequest::default()
        };
        assert!(!pcm.opus_detect());
        assert!(!SpeechRequest::default().opus_detect());

        // Go never writes filteredSamples[0], so the first output sample is 0,
        // and the x5 gain clips before the recurrence ever runs.
        let data: Vec<u8> = [20000i16, 1000, 1000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = high_pass_filter(&data);
        assert_eq!(out.len(), 6);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 0);
        // A trailing odd byte is Go's binary.Read error, which returns nil.
        assert!(high_pass_filter(&[0x01]).is_empty());
    }
}
