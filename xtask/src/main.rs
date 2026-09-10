//! Repo maintenance tasks. Currently: `sync-assets` and `go-probe`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Go programs under `docs/phases/` whose recorded stdout the Rust tests read
/// through `include_str!`. Each is a directory holding `main.go`, its own
/// `go.mod` so it never joins this workspace or the Go server's module, and the
/// `expected.txt` this command writes and checks. Paths are relative to the
/// repo root and are written with `/` because they are joined, never printed
/// raw.
const PROBE_DIRS: &[&str] = &[
    "docs/phases/P1-robot-connect-auth/go-probe",
    "docs/phases/P1-robot-connect-auth/ini-probe",
];

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
        Some("go-probe") => {
            let mut check = false;
            for a in &args[1..] {
                match a.as_str() {
                    "--check" => check = true,
                    other => die(&format!("unknown flag {other}")),
                }
            }
            go_probe(check);
        }
        _ => die(concat!(
            "usage: cargo xtask sync-assets --from <go-repo-root> [--check]\n",
            "       cargo xtask go-probe [--check]"
        )),
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

/// Runs every Go probe under `PROBE_DIRS` and either rewrites its
/// `expected.txt` or checks the committed recording against a fresh run.
///
/// With no `go` on the PATH this prints a SKIP line and returns without
/// failing, so a CI runner that carries no Go toolchain stays green: the
/// recordings are committed artifacts and only regenerating or auditing them
/// needs Go.
fn go_probe(check: bool) {
    match std::process::Command::new("go").arg("version").output() {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "go-probe: SKIP (no `go` on PATH). The expected.txt recordings are committed, \
                 so only regenerating or auditing them needs a Go toolchain."
            );
            return;
        }
        Err(e) => die(&format!("go-probe: could not run `go version`: {e}")),
    }

    let root = repo_root();
    let mut drift = 0usize;
    for rel in PROBE_DIRS {
        let dir = root.join(rel);
        let produced = run_probe(&dir, rel);
        let expected = dir.join("expected.txt");
        if check {
            let recorded = std::fs::read_to_string(&expected).unwrap_or_else(|e| {
                die(&format!(
                    "go-probe --check: cannot read {rel}/expected.txt: {e}"
                ))
            });
            if let Some((n, want, got)) = first_difference(&recorded, &produced) {
                eprintln!("go-probe --check: {rel}/expected.txt differs at line {n}");
                eprintln!("  recorded: {want}");
                eprintln!("  produced: {got}");
                drift += 1;
            }
        } else {
            std::fs::write(&expected, &produced).unwrap_or_else(|e| {
                die(&format!("go-probe: cannot write {rel}/expected.txt: {e}"))
            });
            let lines = produced.lines().count();
            println!("go-probe: wrote {rel}/expected.txt ({lines} lines)");
        }
    }

    if check {
        if drift > 0 {
            eprintln!(
                "go-probe --check: {drift} recording(s) out of date; \
                 rerun `cargo run -p xtask -- go-probe`"
            );
            std::process::exit(1);
        }
        println!("go-probe: clean ({} probe(s))", PROBE_DIRS.len());
    }
}

/// `go run .` in one probe directory, returning its stdout. Each probe carries
/// its own `go.mod`, so this never touches the Rust workspace or the Go
/// server's module.
fn run_probe(dir: &Path, rel: &str) -> String {
    let out = std::process::Command::new("go")
        .args(["run", "."])
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| die(&format!("go-probe: cannot run `go run .` in {rel}: {e}")));
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        die(&format!("go-probe: `go run .` failed in {rel}"));
    }
    String::from_utf8(out.stdout)
        .unwrap_or_else(|e| die(&format!("go-probe: {rel} produced non-UTF-8 output: {e}")))
}

/// The 1-based number of the first line that differs, with both sides rendered
/// through `{:?}` so a stray carriage return or trailing space is visible.
///
/// The split is on `'\n'` rather than `str::lines` on purpose: `lines` swallows
/// a trailing newline, so a recording that differs only by its last byte would
/// compare equal. Splitting leaves that difference as a final empty element on
/// one side, reported as `<end of file>` on the other.
fn first_difference(recorded: &str, produced: &str) -> Option<(usize, String, String)> {
    let render = |s: Option<&str>| match s {
        Some(s) => format!("{s:?}"),
        None => "<end of file>".to_string(),
    };
    let a: Vec<&str> = recorded.split('\n').collect();
    let b: Vec<&str> = produced.split('\n').collect();
    for n in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(n).copied(), b.get(n).copied());
        if x != y {
            return Some((n + 1, render(x), render(y)));
        }
    }
    None
}
