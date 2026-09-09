//! Repo maintenance tasks. Currently: `sync-assets`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// (source path relative to --from, destination relative to assets/)
const ASSET_MAP: &[(&str, &str)] = &[
    ("chipper/webroot", "webroot"),
    ("chipper/intent-data", "intent-data"),
    ("chipper/weather-map.json", "weather-map.json"),
    ("chipper/epod/ep.crt", "epod/ep.crt"),
    ("chipper/epod/ep.key", "epod/ep.key"),
    ("chipper/stttest.pcm", "stttest.pcm"),
    ("vector-cloud/pod-bot-install.sh", "pod-bot-install.sh"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sync-assets") => {
            let mut from: Option<PathBuf> = None;
            let mut check = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--from" => from = it.next().map(PathBuf::from),
                    "--check" => check = true,
                    other => die(&format!("unknown flag {other}")),
                }
            }
            let from = from.unwrap_or_else(|| die("--from <go-repo-root> is required"));
            let assets = repo_root().join("assets");
            let report = sync_assets(&from, &assets, check);
            if check {
                if report.drift > 0 {
                    eprintln!("sync-assets --check: {} file(s) drifted", report.drift);
                    std::process::exit(1);
                }
                println!("sync-assets: clean ({} mapped roots)", ASSET_MAP.len());
            } else {
                println!(
                    "sync-assets: {} file(s) copied ({} mapped roots)",
                    report.copied,
                    ASSET_MAP.len()
                );
            }
        }
        _ => die("usage: cargo xtask sync-assets --from <go-repo-root> [--check]"),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

fn repo_root() -> PathBuf {
    // xtask always runs via `cargo run -p xtask` whose CWD is the workspace root,
    // but resolve from the manifest dir to be safe.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// What one run found. `drift` counts every missing, differing or unexpected
/// file, which is what `--check` fails on; `copied` counts the subset a
/// non-check run actually wrote, which is always zero under `--check`. The two
/// differ because an unexpected file under `assets/` is drift that no copy can
/// resolve.
struct SyncReport {
    drift: usize,
    copied: usize,
}

/// Reports the drift, and when `check` is false copies the drifted files and
/// rewrites the manifest.
fn sync_assets(from: &Path, assets: &Path, check: bool) -> SyncReport {
    let mut expected: BTreeMap<PathBuf, PathBuf> = BTreeMap::new(); // dest rel -> source abs
    for (src_rel, dst_rel) in ASSET_MAP {
        let src = from.join(src_rel);
        if src.is_dir() {
            for e in walkdir::WalkDir::new(&src)
                .into_iter()
                .filter_map(Result::ok)
            {
                if e.file_type().is_file() {
                    let sub = e.path().strip_prefix(&src).unwrap();
                    expected.insert(Path::new(dst_rel).join(sub), e.path().to_path_buf());
                }
            }
        } else if src.is_file() {
            expected.insert(PathBuf::from(dst_rel), src);
        } else {
            eprintln!("warning: source missing: {}", src.display());
        }
    }

    let mut drift = 0usize;
    let mut copied = 0usize;
    for (dst_rel, src) in &expected {
        let dst = assets.join(dst_rel);
        let differs = match (hash_file(src), hash_file(&dst)) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        };
        if differs {
            drift += 1;
            if check {
                eprintln!("drift: {}", dst_rel.display());
            } else {
                std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                std::fs::copy(src, &dst).unwrap();
                copied += 1;
                eprintln!("copied: {}", dst_rel.display());
            }
        }
    }

    // Files present under assets/ but not expected (manifest itself is exempt).
    for e in walkdir::WalkDir::new(assets)
        .into_iter()
        .filter_map(Result::ok)
    {
        if e.file_type().is_file() {
            let rel = e.path().strip_prefix(assets).unwrap().to_path_buf();
            if rel != Path::new("MANIFEST.sha256") && !expected.contains_key(&rel) {
                drift += 1;
                eprintln!("unexpected file under assets/: {}", rel.display());
            }
        }
    }

    if !check {
        let mut manifest = String::new();
        for dst_rel in expected.keys() {
            let h = hash_file(&assets.join(dst_rel)).unwrap();
            let unix = dst_rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            manifest.push_str(&format!("{h}  ./{unix}\n"));
        }
        std::fs::write(assets.join("MANIFEST.sha256"), manifest).unwrap();
    }
    SyncReport { drift, copied }
}

fn hash_file(p: &Path) -> Option<String> {
    let data = std::fs::read(p).ok()?;
    let mut h = Sha256::new();
    h.update(&data);
    Some(format!("{:x}", h.finalize()))
}
