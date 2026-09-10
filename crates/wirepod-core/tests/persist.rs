//! Atomic persistence: the file is replaced, never rewritten, and a failure
//! leaves neither a damaged target nor a temporary behind.
//!
//! Everything here runs in a directory under the system temporary directory,
//! named for the process, so nothing touches the repository or the live
//! `%APPDATA%\wire-pod` the Go server is serving from.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wirepod_core::persist::write_atomic;

/// Go's mode for the bot-info and jdocs files (`jdocs/server.go:52`,
/// `vars.go:317`), used here as a representative one.
const MODE: u32 = 0o644;

/// A ceiling on the concurrent case, generous enough that only a deadlock
/// reaches it. Real durations, because this crate's tests never pause the
/// runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

/// A directory under the system temporary directory, removed when the test
/// ends.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is before the epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wirepod-persist-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The names in a directory, sorted, so a leftover temporary is visible.
fn entry_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("could not list the directory")
        .map(|entry| {
            entry
                .expect("could not read a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_write_creates_the_file_then_replaces_it_and_leaves_no_temporary() {
    let directory = TempDir::new("replace");
    let target = directory.path().join("botSdkInfo.json");

    write_atomic(&target, b"first".to_vec(), MODE)
        .await
        .expect("the first write failed");
    assert_eq!(fs::read(&target).expect("the file is missing"), b"first");
    assert_eq!(entry_names(directory.path()), ["botSdkInfo.json"]);

    write_atomic(&target, b"second and longer".to_vec(), MODE)
        .await
        .expect("the second write failed");
    assert_eq!(
        fs::read(&target).expect("the file is missing"),
        b"second and longer"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["botSdkInfo.json"],
        "a temporary survived a successful write"
    );
}

/// The property the whole module exists for: the target is swapped, not
/// truncated and refilled.
///
/// A second hard link to the original file is a witness. A rename unlinks the
/// name and leaves the witness pointing at the old bytes; a write in place
/// would change what the witness reads, because it and the target would still
/// be the same file.
#[tokio::test]
async fn replacing_a_file_swaps_it_rather_than_rewriting_it_in_place() {
    let directory = TempDir::new("swap");
    let target = directory.path().join("jdocs.json");
    let witness = directory.path().join("witness.json");

    fs::write(&target, b"original").expect("could not seed the target");
    fs::hard_link(&target, &witness).expect("the temporary filesystem has no hard links");

    write_atomic(&target, b"replacement".to_vec(), MODE)
        .await
        .expect("the write failed");

    assert_eq!(
        fs::read(&target).expect("the target is missing"),
        b"replacement"
    );
    assert_eq!(
        fs::read(&witness).expect("the witness is missing"),
        b"original",
        "the file was rewritten in place instead of being replaced"
    );
}

/// A rename that cannot succeed: the target name is already a directory, which
/// neither platform will let a file replace.
///
/// The temporary is created before the rename is attempted, so this is the case
/// that proves the failure path removes it.
#[tokio::test]
async fn a_rename_that_cannot_succeed_leaves_the_target_and_no_temporary() {
    let directory = TempDir::new("collide");
    let target = directory.path().join("jdocs.json");
    fs::create_dir(&target).expect("could not create the colliding directory");
    fs::write(target.join("marker"), b"keep").expect("could not seed the marker");

    write_atomic(&target, b"replacement".to_vec(), MODE)
        .await
        .expect_err("renaming a file over a directory must fail");

    assert!(target.is_dir(), "the directory did not survive");
    assert_eq!(
        fs::read(target.join("marker")).expect("the marker is missing"),
        b"keep"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["jdocs.json"],
        "the temporary was left behind after a failed rename"
    );
}

#[tokio::test]
async fn a_target_whose_directory_does_not_exist_reports_the_error() {
    let directory = TempDir::new("missing");
    let target = directory.path().join("nope").join("jdocs.json");

    let error = write_atomic(&target, b"body".to_vec(), MODE)
        .await
        .expect_err("writing into a missing directory must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        entry_names(directory.path()).is_empty(),
        "something was created anyway"
    );
}

/// Go's per-call-site mode reaches the new file, masked by the umask the way
/// `os.WriteFile` is. The assertion is that no bit beyond the requested ones is
/// set, which is umask independent.
#[cfg(unix)]
#[tokio::test]
async fn the_requested_mode_reaches_the_new_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new("mode");

    for mode in [0o644u32, 0o755, 0o600] {
        let target = directory.path().join(format!("file-{mode:o}"));
        write_atomic(&target, b"body".to_vec(), mode)
            .await
            .expect("the write failed");

        let got = fs::metadata(&target)
            .expect("the file is missing")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            got & !mode,
            0,
            "mode {got:o} carries a bit {mode:o} did not ask for"
        );
        assert_ne!(got & 0o400, 0, "the owner cannot read mode {got:o}");
    }
}

/// A failure before the temporary exists: the directory refuses new files, so
/// creating it fails and the target is never touched.
#[cfg(unix)]
#[tokio::test]
async fn a_write_into_a_read_only_directory_leaves_the_original_and_no_temporary() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new("readonly");
    let target = directory.path().join("jdocs.json");
    fs::write(&target, b"original").expect("could not seed the target");

    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555))
        .expect("could not make the directory read only");

    // Root ignores the directory mode, so establish that the mode really does
    // block a write before asserting that it blocks this one.
    let probe = directory.path().join("probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("could not restore the directory");
        eprintln!("skipped: this user can write into a read-only directory");
        return;
    }

    let result = write_atomic(&target, b"replacement".to_vec(), MODE).await;

    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
        .expect("could not restore the directory");

    result.expect_err("writing into a read-only directory must fail");
    assert_eq!(
        fs::read(&target).expect("the target is missing"),
        b"original",
        "the original did not survive a failed write"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["jdocs.json"],
        "the temporary was left behind"
    );
}

/// A failure after the temporary exists, in the shape Windows really produces:
/// another handle holds the target with no sharing, so the rename is refused
/// however many times it is retried.
#[cfg(windows)]
#[tokio::test]
async fn a_rename_blocked_by_an_open_handle_leaves_the_original_and_no_temporary() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    let directory = TempDir::new("locked");
    let target = directory.path().join("jdocs.json");
    fs::write(&target, b"original").expect("could not seed the target");

    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&target)
        .expect("could not hold the target open");

    write_atomic(&target, b"replacement".to_vec(), MODE)
        .await
        .expect_err("a rename over a file held without sharing must fail");

    drop(held);

    assert_eq!(
        fs::read(&target).expect("the target is missing"),
        b"original",
        "the original did not survive a failed rename"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["jdocs.json"],
        "the temporary was left behind after a failed rename"
    );
}

/// Concurrent writers of one file, which is the shape Go's five bot-info
/// writers have. Each writer's bytes are uniform and distinct, so a file that
/// mixes two of them, or that is short, is a torn write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_never_leave_a_torn_file() {
    const WRITERS: u8 = 8;
    const LENGTH: usize = 64 * 1024;

    let directory = TempDir::new("concurrent");
    let target = directory.path().join("botSdkInfo.json");
    fs::write(&target, b"original").expect("could not seed the target");

    let writes = tokio::time::timeout(CEILING, async {
        let mut handles = Vec::new();
        for writer in 0..WRITERS {
            let target = target.clone();
            handles.push(tokio::spawn(async move {
                write_atomic(target, vec![b'a' + writer; LENGTH], MODE).await
            }));
        }
        for handle in handles {
            handle
                .await
                .expect("a writer panicked")
                .expect("a writer failed");
        }
    })
    .await;
    writes.expect("the writers did not finish within the ceiling");

    let written = fs::read(&target).expect("the target is missing");
    assert_eq!(written.len(), LENGTH, "the file is a partial write");
    let first = written[0];
    assert!(
        (b'a'..b'a' + WRITERS).contains(&first),
        "the file starts with a byte no writer wrote"
    );
    assert!(
        written.iter().all(|byte| *byte == first),
        "the file mixes two writers' bytes"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["botSdkInfo.json"],
        "a temporary survived the concurrent writes"
    );
}
