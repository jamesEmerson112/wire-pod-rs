//! Translation of `pkg/wirepod/localization/download.go`.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use wirepod_core::AppState;
use wirepod_core::config::write_config_to_disk;

pub const URL_PREFIX: &str = "https://github.com/kercre123/vosk-models/raw/main/";

//pub const URL_PREFIX: &str = "https://alphacephei.com/vosk/models/";

// Go keeps this in the package global `DownloadStatus`.
#[derive(Clone)]
pub struct DownloadStatus(Arc<Mutex<String>>);

impl Default for DownloadStatus {
    fn default() -> Self {
        Self(Arc::new(Mutex::new("not downloading".to_string())))
    }
}

impl DownloadStatus {
    pub fn get(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, status: impl Into<String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = status.into();
    }
}

pub async fn download_vosk_model(state: &AppState, status: &DownloadStatus, language: &str) {
    let mut filename = "vosk-model-small-".to_string();
    filename += match language {
        "en-US" => "en-us-0.15.zip",
        "it-IT" => "it-0.22.zip",
        "es-ES" => "es-0.42.zip",
        "fr-FR" => "fr-0.22.zip",
        "de-DE" => "de-0.15.zip",
        "pt-BR" => "pt-0.3.zip",
        "pl-PL" => "pl-0.22.zip",
        "zh-CN" => "cn-0.22.zip",
        "tr-TR" => "tr-0.3.zip",
        "ru-RU" => "ru-0.22.zip",
        "nt-NL" => "nl-0.22.zip",
        "uk-UA" => "uk-v3-small.zip",
        "vi-VN" => "vn-0.4.zip",
        "ko-KR" => "ko-0.22.zip",
        _ => {
            tracing::info!(comp = "", "Language not valid? {language}");
            return;
        }
    };
    let vosk_model_path = state.paths().data().vosk_model_dir();
    let _ = std::fs::create_dir_all(&vosk_model_path);
    let url = format!("{URL_PREFIX}{filename}");
    let filep = std::env::temp_dir().join(&filename);
    let destpath = vosk_model_path.join(language);
    download_file(status, &url, &filep).await;
    {
        let (status, filep, destpath) = (status.clone(), filep.clone(), destpath.clone());
        let _ = tokio::task::spawn_blocking(move || unzip_file(&status, &filep, &destpath)).await;
    }
    let _ = std::fs::rename(
        destpath.join(filename.trim_end_matches(".zip")),
        destpath.join("model"),
    );
    let _ = std::fs::remove_file(&filep);
    // TODO(M3): vars.DownloadedVoskModels = append(vars.DownloadedVoskModels, language)
    status.set("Reloading voice processor");
    state.update_config(|config| {
        config.stt.language = language.to_string();
        config.past_initial_setup = true;
    });
    if let Err(err) = write_config_to_disk(&state.config(), state.config_gate()).await {
        tracing::info!(comp = "", "{err}");
    }
    // TODO(M3): ReloadVosk()
    tracing::info!(comp = "", "Reloaded voice processor successfully");
    status.set("success");
}

pub async fn print_download_percent(
    status: DownloadStatus,
    mut done: oneshot::Receiver<u64>,
    path: std::path::PathBuf,
    total: u64,
) {
    let mut stop = false;
    loop {
        if done.try_recv().is_ok() {
            stop = true;
        } else {
            // Go calls log.Fatal when the file cannot be opened; here the
            // progress report just stops.
            let Ok(fi) = std::fs::metadata(&path) else {
                return;
            };
            let mut size = fi.len();
            if size == 0 {
                size = 1;
            }
            let percent = size as f64 / total as f64 * 100.0;
            let show_percent = percent.floor();
            // Go prints an infinite float as "+Inf".
            let shown = if show_percent.is_infinite() {
                "+Inf".to_string()
            } else {
                format!("{show_percent}")
            };
            status.set(format!("Model download status: {shown}%"));
        }
        if stop {
            status.set("completed");
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn download_file(status: &DownloadStatus, url: &str, dest: &Path) {
    let current = status.get();
    if current.contains("success")
        || current.contains("error")
        || current.contains("not downloading")
    {
        // Go turns certificate verification off on the default transport.
        let client = match reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                tracing::info!(comp = "", "{err}");
                status.set(format!("error: {err}"));
                return;
            }
        };
        tracing::info!(comp = "", "Downloading {url} to {}", dest.display());
        let mut out = match std::fs::File::create(dest) {
            Ok(out) => out,
            Err(err) => {
                tracing::info!(comp = "", "{err}");
                status.set(format!("error: {err}"));
                return;
            }
        };
        let head_resp = match client.head(url).send().await {
            Ok(resp) => resp,
            Err(err) => {
                tracing::info!(comp = "", "{err}");
                status.set(format!("error: {err}"));
                return;
            }
        };
        let size = head_resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let (done_tx, done_rx) = oneshot::channel();
        let printer = tokio::spawn(print_download_percent(
            status.clone(),
            done_rx,
            dest.to_path_buf(),
            size,
        ));
        let mut resp = match client.get(url).send().await {
            Ok(resp) => resp,
            Err(err) => {
                status.set(format!("error: {err}"));
                tracing::info!(comp = "", "{err}");
                return;
            }
        };
        let mut n = 0u64;
        while let Ok(Some(chunk)) = resp.chunk().await {
            if out.write_all(&chunk).is_err() {
                break;
            }
            n += chunk.len() as u64;
        }
        let _ = done_tx.send(n);
        let _ = printer.await;
        status.set("Completed download");
    } else {
        tracing::info!(
            comp = "",
            "Not downloading model because download is currently happening"
        );
    }
}

pub fn unzip_file(status: &DownloadStatus, file: &Path, dest: &Path) {
    let mut zip_reader = match std::fs::File::open(file)
        .map_err(zip::result::ZipError::Io)
        .and_then(zip::ZipArchive::new)
    {
        Ok(zip_reader) => zip_reader,
        Err(err) => {
            status.set(format!("error downloading: {err}"));
            tracing::info!(comp = "", "error opening zip file: {err}");
            return;
        }
    };
    status.set("Unpacking model...");

    for i in 0..zip_reader.len() {
        let mut rc = match zip_reader.by_index(i) {
            Ok(rc) => rc,
            Err(err) => {
                status.set(format!("error downloading: {err}"));
                tracing::info!(comp = "", "error opening zip file: {err}");
                return;
            }
        };

        // Go joins the raw entry name; an entry that would leave `dest` is skipped here.
        let Some(name) = rc.enclosed_name() else {
            continue;
        };
        let path = dest.join(name);
        // Go applies the entry's file mode; the platform default is used here.
        if rc.is_dir() {
            let _ = std::fs::create_dir_all(&path);
        } else {
            let mut f = match std::fs::File::create(&path) {
                Ok(f) => f,
                Err(err) => {
                    status.set(format!("error downloading: {err}"));
                    tracing::info!(comp = "", "Error creating file: {err}");
                    return;
                }
            };

            if let Err(err) = std::io::copy(&mut rc, &mut f) {
                status.set(format!("error downloading: {err}"));
                tracing::info!(comp = "", "Error writing to file: {err}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_archive_unpacks_under_the_destination() {
        let dir = std::env::temp_dir().join(format!("wirepod-unzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("model.zip");
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            writer
                .add_directory("vosk-model-small-xx/", options)
                .unwrap();
            writer
                .start_file("vosk-model-small-xx/README", options)
                .unwrap();
            writer.write_all(b"model").unwrap();
            writer.finish().unwrap();
        }

        let status = DownloadStatus::default();
        let dest = dir.join("xx-XX");
        std::fs::create_dir_all(&dest).unwrap();
        unzip_file(&status, &archive, &dest);

        assert_eq!(status.get(), "Unpacking model...");
        assert_eq!(
            std::fs::read(dest.join("vosk-model-small-xx/README")).unwrap(),
            b"model"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_second_download_is_refused_while_one_is_running() {
        let status = DownloadStatus::default();
        status.set("Model download status: 40%");
        let dest = std::env::temp_dir().join("wirepod-never-created.zip");
        download_file(&status, "https://127.0.0.1:1/never", &dest).await;
        assert_eq!(status.get(), "Model download status: 40%");
        assert!(!dest.exists());
    }
}
