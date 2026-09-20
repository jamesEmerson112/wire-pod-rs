//! Translation of `pkg/wirepod/stt/vosk/Vosk.go`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use vosk::{LogLevel, Model, Recognizer};
use wirepod_audio::{SpeechRequest, bytes_to_samples, split_vad};
use wirepod_core::intents::{CustomIntent, JsonIntent};

use crate::vosk_context::get_grammer_list;
use crate::{SttEngine, SttError};

pub const NAME: &str = "vosk";

/// What Go reads off `vars.APIConfig` and the `vars` globals at each call site.
#[derive(Clone, Debug, Default)]
pub struct VoskConfig {
    pub past_initial_setup: bool,
    pub stt_language: String,
    pub intent_graph: bool,
    pub vosk_model_path: PathBuf,
    pub stttest_path: PathBuf,
    pub intent_list: Vec<JsonIntent>,
    pub custom_intents: Vec<CustomIntent>,
}

/// Go's `ARec`. `InUse` is the slot being empty here: a recognizer handed out is
/// taken out of the pool and put back when the caller is done with it, which is
/// the same thing Go's flag plus a returned pointer say. The model rides along
/// so that `Init` freeing it cannot pull the ground out from under a recognizer
/// that is still in flight, which is what Go does.
struct ARec {
    rec: Recognizer,
    _model: Arc<Model>,
}

#[derive(Default)]
struct Recs {
    model: Option<Arc<Model>>,
    model_loaded: bool,
    grammer: Vec<String>,
    grm_recs: Vec<Option<ARec>>,
    gp_recs: Vec<Option<ARec>>,
}

/// Go's package globals, which are one engine.
pub struct Vosk {
    grammer_enable: AtomicBool,
    config: VoskConfig,
    recsmu: Mutex<Recs>,
}

impl Vosk {
    pub fn new(config: VoskConfig) -> Self {
        Self {
            grammer_enable: AtomicBool::new(false),
            config,
            recsmu: Mutex::new(Recs::default()),
        }
    }

    pub fn grammer_enable(&self) -> bool {
        self.grammer_enable.load(Ordering::Relaxed)
    }

    fn init(&self) -> Result<(), SttError> {
        if std::env::var("VOSK_WITH_GRAMMER").as_deref() == Ok("true") {
            println!("Initializing vosk with grammer optimizations");
            self.grammer_enable.store(true, Ordering::Relaxed);
        }
        if self.config.past_initial_setup {
            // Go's vosk.SetLogLevel(-1), which is this crate's Warn.
            vosk::set_log_level(LogLevel::Warn);
            let mut recs = self.recsmu.lock().expect("vosk recognizer pool");
            if recs.model_loaded {
                tracing::debug!(
                    target: "stt",
                    "a model was already loaded, freeing all recognizers and model"
                );
                recs.gp_recs.clear();
                recs.grm_recs.clear();
                recs.model = None;
            }
            let mut stt_language = self.config.stt_language.clone();
            if stt_language.is_empty() {
                stt_language = "en-US".to_string();
            }
            let model_path = self.config.vosk_model_path.join(stt_language).join("model");
            if let Err(err) = std::fs::metadata(&model_path) {
                println!("Path does not exist: {}", model_path.display());
                return Err(SttError::new(err.to_string()));
            }
            tracing::debug!(target: "stt", "opening VOSK model ({})", model_path.display());
            // Go calls log.Fatal on a model it cannot open and never reaches the
            // return below it.
            let mut a_model = Model::new(model_path.to_string_lossy().into_owned())
                .ok_or_else(|| SttError::new("could not open VOSK model"))?;
            if self.grammer_enable() {
                tracing::debug!(target: "stt", "initializing grammer list");
                recs.grammer = get_grammer_list(
                    &mut a_model,
                    &self.config.stt_language,
                    &self.config.intent_list,
                    &self.config.custom_intents,
                );
            }
            let a_model = Arc::new(a_model);
            recs.model = Some(Arc::clone(&a_model));

            tracing::debug!(target: "stt", "initializing VOSK recognizers");
            if self.grammer_enable() {
                let grm_recognizer = Recognizer::new_with_grammar(&a_model, 16000.0, &recs.grammer)
                    .ok_or_else(|| SttError::new("could not create VOSK recognizer"))?;
                recs.grm_recs.push(Some(ARec {
                    rec: grm_recognizer,
                    _model: Arc::clone(&a_model),
                }));
            }
            let gp_recognizer = Recognizer::new(&a_model, 16000.0)
                .ok_or_else(|| SttError::new("could not create VOSK recognizer"))?;
            recs.gp_recs.push(Some(ARec {
                rec: gp_recognizer,
                _model: a_model,
            }));
            recs.model_loaded = true;
            drop(recs);
            tracing::debug!(target: "stt", "VOSK initiated successfully");
            self.run_test();
        }
        Ok(())
    }

    fn run_test(&self) {
        // make sure recognizer is all loaded into RAM
        tracing::debug!(target: "stt", "running recognizer test");
        let with_grm = if self.grammer_enable() {
            tracing::debug!(target: "stt", "using grammer-optimized recognizer");
            true
        } else {
            tracing::debug!(target: "stt", "using general recognizer");
            false
        };
        let (mut rec, recind) = match self.get_rec(with_grm) {
            Ok(rec) => rec,
            Err(err) => {
                tracing::error!(target: "stt", "{err}");
                return;
            }
        };
        let pcm_bytes = std::fs::read(&self.config.stttest_path).unwrap_or_default();
        let c_time = Instant::now();
        let mic_data = split_vad(&pcm_bytes);
        for sample in mic_data {
            let _ = rec.rec.accept_waveform(&bytes_to_samples(sample));
        }
        let jres = rec.rec.final_result().single().map(|r| r.text.to_string());
        self.put_rec(with_grm, recind, rec);
        // Go asserts the "text" key straight out of the decoded map.
        let Some(transcribed_text) = jres else {
            tracing::error!(target: "stt", "no text in vosk result");
            return;
        };
        let t_time = c_time.elapsed();
        tracing::debug!(target: "stt", "text (from test): {transcribed_text}");
        if t_time.as_secs_f64() > 3.0 {
            tracing::debug!(
                target: "stt",
                "Vosk test took a while, performance may be degraded. ({t_time:?})"
            );
        }
        tracing::debug!(target: "stt", "Vosk test successful! (Took {t_time:?})");
    }

    fn get_rec(&self, with_grm: bool) -> Result<(ARec, usize), SttError> {
        let mut recs = self.recsmu.lock().expect("vosk recognizer pool");
        if with_grm && self.grammer_enable() {
            for ind in 0..recs.grm_recs.len() {
                if let Some(rec) = recs.grm_recs[ind].take() {
                    return Ok((rec, ind));
                }
            }
        } else {
            for ind in 0..recs.gp_recs.len() {
                if let Some(rec) = recs.gp_recs[ind].take() {
                    return Ok((rec, ind));
                }
            }
        }
        let model = recs.model.clone();
        let grammer = recs.grammer.clone();
        // Go unlocks by hand here, allocates, and takes the lock again.
        drop(recs);
        let Some(model) = model else {
            return Err(SttError::new("no VOSK model loaded"));
        };
        let new_rec = if with_grm {
            Recognizer::new_with_grammar(&model, 16000.0, &grammer)
        } else {
            Recognizer::new(&model, 16000.0)
        };
        // Go calls log.Fatal here.
        let Some(new_rec) = new_rec else {
            return Err(SttError::new("could not create VOSK recognizer"));
        };
        let newrec = ARec {
            rec: new_rec,
            _model: model,
        };
        let mut recs = self.recsmu.lock().expect("vosk recognizer pool");
        if with_grm {
            recs.grm_recs.push(None);
            Ok((newrec, recs.grm_recs.len() - 1))
        } else {
            recs.gp_recs.push(None);
            Ok((newrec, recs.gp_recs.len() - 1))
        }
    }

    /// Go's `grmRecs[recind].InUse = false`, which panics with index out of
    /// range if `Init` emptied the pool while the recognizer was out.
    fn put_rec(&self, with_grm: bool, recind: usize, rec: ARec) {
        let mut recs = self.recsmu.lock().expect("vosk recognizer pool");
        let pool = if with_grm {
            &mut recs.grm_recs
        } else {
            &mut recs.gp_recs
        };
        if recind >= pool.len() {
            tracing::error!(target: "stt", "recognizer {recind} no longer has a slot");
            return;
        }
        pool[recind] = Some(rec);
    }
}

#[async_trait]
impl SttEngine for Vosk {
    fn name(&self) -> &str {
        NAME
    }

    fn init(&self) -> Result<(), SttError> {
        Vosk::init(self)
    }

    async fn stt(&self, mut req: SpeechRequest) -> Result<String, SttError> {
        tracing::debug!(target: "stt", bot = %req.device, "vosk: processing");
        let with_grm = if (self.config.intent_graph || req.is_kg) || !self.grammer_enable() {
            tracing::info!(comp = "", "Using general recognizer");
            false
        } else {
            tracing::info!(comp = "", "Using grammer-optimized recognizer");
            true
        };
        let (mut rec, recind) = self.get_rec(with_grm)?;
        rec.rec.set_words(true);
        let _ = rec.rec.accept_waveform(&bytes_to_samples(&req.first_req));
        req.detect_end_of_speech();
        loop {
            // Go returns here without clearing InUse, so the slot is lost. Kept:
            // the recognizer is dropped and its slot stays empty.
            let chunk = req
                .get_next_stream_chunk()
                .await
                .map_err(|err| SttError::new(err.to_string()))?;
            let (speech_is_done, do_process) = req.detect_end_of_speech();
            if do_process {
                let _ = rec.rec.accept_waveform(&bytes_to_samples(&chunk));
            }
            if speech_is_done {
                break;
            }
        }
        let jres = rec.rec.final_result().single().map(|r| r.text.to_string());
        self.put_rec(with_grm, recind, rec);
        let Some(transcribed_text) = jres else {
            return Err(SttError::new("no text in vosk result"));
        };
        tracing::info!(target: "stt", bot = %req.device, "transcribed: {transcribed_text}");
        Ok(transcribed_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine is built but never initialized, so nothing here opens a model.
    #[test]
    fn an_uninitialized_engine_refuses_rather_than_panicking() {
        let vosk = Vosk::new(VoskConfig::default());
        assert_eq!(vosk.name(), "vosk");
        assert!(!vosk.is_sti());
        assert!(!vosk.grammer_enable());
        // No model is loaded, so Go's nil dereference in getRec is this error.
        assert!(vosk.get_rec(false).is_err());
        // PastInitialSetup false is Go's "do nothing and answer nil".
        assert!(Vosk::init(&vosk).is_ok());
    }
}
