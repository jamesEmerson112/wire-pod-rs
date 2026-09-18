//! Atomic persistence: the file is replaced, never rewritten, and a failure
//! leaves neither a damaged target nor a temporary behind.
//!
//! The one thing that does leave a temporary behind is a process that dies
//! between creating it and renaming it, and where that temporary lands is a
//! parity question rather than a tidiness one, because Go reads every name in
//! `session-certs/` as a robot ESN (`vars.go:376-388`). The session-certificate
//! cases below are that, and the boot sweep is what clears up after it.
//!
//! Everything here runs in a directory under the system temporary directory,
//! named for the process, so nothing touches the repository or the live
//! `%APPDATA%\wire-pod` the Go server is serving from. No certificate or key
//! is written anywhere: a PEM header is all these cases need.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wirepod_core::persist::{TemporaryIn, sweep_temporaries, write_atomic};
use wirepod_core::test_support::abort_write_after_filling_the_temporary;

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
///
/// Unix only, because the mode is: Windows has no permission bit set for the
/// assertion to read.
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

/// The other half of `os.WriteFile`'s mode handling: a file that already exists
/// keeps the mode it already had, whatever the call site asks for.
///
/// Go hands the mode to an `O_CREATE` open (`os/file.go:849-859`), and `open(2)`
/// applies a mode only when it creates the file. The shape that makes this
/// observable is Go's own: four writers create `botSdkInfo.json` at `0644`
/// (`jdocs/server.go:52`, `:77`, `token/token.go:95`,
/// `jdocs/botInfoStorer.go:152`) and the pinger writes the same file at `0777`
/// (`sdkapp/jdocspinger.go:250`), where in Go that `0777` never reaches the
/// disk.
///
/// Unix only, because the mode is. `0640` is set with `chmod`, which no umask
/// touches, so the expected value is exact.
#[cfg(unix)]
#[tokio::test]
async fn a_file_that_already_exists_keeps_its_own_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new("keepmode");
    let target = directory.path().join("botSdkInfo.json");

    write_atomic(&target, b"created".to_vec(), 0o644)
        .await
        .expect("the creating write failed");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640))
        .expect("could not set the mode the file is to keep");

    write_atomic(&target, b"replaced".to_vec(), 0o777)
        .await
        .expect("the replacing write failed");

    assert_eq!(
        fs::read(&target).expect("the target is missing"),
        b"replaced"
    );
    let got = fs::metadata(&target)
        .expect("the target is missing")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        got, 0o640,
        "a write re-stamped mode {got:o} onto a file that already existed"
    );
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

/// The case the Windows retry exists for: another handle holds the target with
/// no sharing for a moment, which on that platform is a virus scanner or a
/// search indexer opening the state file just after it changed, and then lets
/// go. The write has to land anyway.
///
/// The budget is the test's rather than production's, and
/// `write_atomic_with_retry_budget` says why: the production budget is shorter
/// than the time this machine takes to create, fill and sync the temporary, so
/// a test using it would be racing its own setup.
///
/// The attempt count is asserted, because without it the test passes vacuously
/// whenever the writer reaches its first rename after the hold has already
/// cleared: the write then lands at once and the retry loop never runs. The
/// hold and the budget are both sized for that assertion rather than for the
/// write alone. Creating, filling and syncing an eleven-byte temporary measures
/// between two and twenty-five milliseconds here, and reaching it also costs a
/// `spawn_blocking` hand-off on a machine running the rest of this suite in
/// parallel; a forty-millisecond hold lost that race about half the time, which
/// is what the vacuous pass looked like.
#[cfg(windows)]
#[tokio::test]
async fn a_hold_that_clears_does_not_lose_the_write() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::mpsc;

    use wirepod_core::test_support::write_atomic_with_retry_budget;

    /// Long enough to outlast creating, filling and syncing the temporary and
    /// the `spawn_blocking` hand-off that precedes it, by more than an order of
    /// magnitude, so the first rename is attempted while the hold is still on.
    const HOLD: Duration = Duration::from_millis(500);
    /// Thirteen attempts backing off from two milliseconds, eight seconds of
    /// waiting in total, so the hold is gone with most of the budget unspent:
    /// the rename lands on the attempt after the cumulative wait passes
    /// [`HOLD`], which is 510 milliseconds in, and four attempts remain behind
    /// it.
    const ATTEMPTS: u32 = 13;
    const FIRST_BACKOFF: Duration = Duration::from_millis(2);

    let directory = TempDir::new("transient");
    let target = directory.path().join("botSdkInfo.json");
    fs::write(&target, b"original").expect("could not seed the target");

    let (opened, is_open) = mpsc::channel();
    let holder = {
        let target = target.clone();
        std::thread::spawn(move || {
            let held = OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&target)
                .expect("could not hold the target open");
            opened.send(()).expect("nobody is waiting for the hold");
            std::thread::sleep(HOLD);
            drop(held);
        })
    };
    is_open.recv().expect("the holder never opened the target");

    let write = tokio::time::timeout(
        CEILING,
        write_atomic_with_retry_budget(
            &target,
            b"replacement".to_vec(),
            MODE,
            ATTEMPTS,
            FIRST_BACKOFF,
        ),
    )
    .await
    .expect("the write did not finish within the ceiling");
    let attempts = write.expect("a hold that clears must not cost the write");

    // Without this the test passes vacuously whenever the writer reaches its
    // first rename after the hold has cleared: the rename then succeeds at once
    // and the retry this test exists for never runs.
    assert!(
        attempts > 1,
        "the write landed on its first rename, so the hold was already gone and the retry was \
         never exercised; reaching the first rename took longer than the {HOLD:?} hold"
    );

    holder.join().expect("the holder panicked");

    assert_eq!(
        fs::read(&target).expect("the target is missing"),
        b"replacement"
    );
    assert_eq!(
        entry_names(directory.path()),
        ["botSdkInfo.json"],
        "a temporary survived a write that had to wait"
    );
}

/// A failure after the temporary exists, in the shape Windows really produces:
/// another handle holds the target with no sharing and never lets go, so the
/// rename is refused however many times it is retried.
///
/// What is asserted about how it ends is that it does: the call comes back with
/// the error rather than waiting on a hold that is never coming, and it comes
/// back on the production budget rather than on the ceiling. How long that
/// budget is is not asserted here, because it cannot be: creating, filling and
/// syncing the temporary costs more on a busy machine than the fourteen
/// milliseconds the whole budget spends waiting, so no elapsed time tells four
/// attempts from one. The unit test beside `RetryBudget` pins the number
/// instead.
#[cfg(windows)]
#[tokio::test]
async fn a_permanently_held_target_reports_an_error_rather_than_spinning() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Instant;

    /// Generous, and far under the ceiling, because the point is only that the
    /// loop ends on its own.
    const NOT_SPINNING: Duration = Duration::from_secs(5);

    let directory = TempDir::new("locked");
    let target = directory.path().join("jdocs.json");
    fs::write(&target, b"original").expect("could not seed the target");

    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&target)
        .expect("could not hold the target open");

    let started = Instant::now();
    let write = tokio::time::timeout(
        CEILING,
        write_atomic(&target, b"replacement".to_vec(), MODE),
    )
    .await
    .expect("the write never gave up at all");
    let elapsed = started.elapsed();

    drop(held);

    write.expect_err("a rename over a file held without sharing must fail");

    assert!(
        elapsed < NOT_SPINNING,
        "the write took {elapsed:?}, so it is waiting for a hold that is never coming"
    );

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

// ---------------------------------------------------------------------------
// What a crash leaves in session-certs/
// ---------------------------------------------------------------------------

/// A PEM header is enough for this test: nothing here parses a certificate, and
/// no real key or certificate may be written anywhere.
const PEM_HEAD: &[u8] = b"-----BEGIN CERTIFICATE-----\n";

/// Go's own loop over `session-certs/`, in Go's shape: every entry whose name
/// is not the literal `placeholder` is taken to be a robot's ESN
/// (`vars.go:376-380`), read, and handed to `pem.Decode` whose nil result is
/// dereferenced unchecked (`vars.go:387-388`).
///
/// So an entry that is not a certificate named after a robot is not untidiness
/// there, it is a panic at boot after a rollback, or a bogus ESN in
/// `RecurringInfo`. This is the assertion the placement rule exists for.
fn every_entry_is_a_bare_target_name(directory: &Path) {
    for name in entry_names(directory) {
        assert!(
            !name.contains(".tmp"),
            "vars.go:380 would read the temporary {name} as a robot ESN"
        );
        assert!(
            name == "placeholder" || name.chars().all(|character| character.is_ascii_hexdigit()),
            "vars.go:380 would read {name} as a robot ESN"
        );
    }
}

/// A write of a session certificate that dies between creating its temporary
/// and renaming it leaves nothing at all in `session-certs/`.
///
/// This is the one directory Go enumerates and reads names out of
/// (`vars.go:371`, reached from `vars.go:256` at every boot), so the temporary
/// has to go somewhere else, and the somewhere else has to be on the same
/// volume for the rename to stay a rename. The pod root is both.
#[tokio::test]
async fn an_aborted_session_certificate_write_leaves_nothing_in_session_certs() {
    let pod = TempDir::new("session-certs");
    let certs = pod.path().join("session-certs");
    fs::create_dir(&certs).expect("could not create session-certs/");
    // The two things Go's loop expects to find there: its own placeholder and a
    // certificate named after a robot.
    fs::write(certs.join("placeholder"), b"").expect("could not seed the placeholder");
    fs::write(certs.join("00303f28"), PEM_HEAD).expect("could not seed a certificate");

    let target = certs.join("00e20100");
    let temporary = tokio::time::timeout(
        CEILING,
        abort_write_after_filling_the_temporary(
            &target,
            PEM_HEAD.to_vec(),
            0o777,
            TemporaryIn::ParentDirectory,
        ),
    )
    .await
    .expect("the aborted write hung")
    .expect("the aborted write failed before it could leave anything");

    assert!(
        temporary.is_file(),
        "the abort left no temporary, so this test proves nothing"
    );
    assert_eq!(
        temporary.parent(),
        Some(pod.path()),
        "the temporary did not land in the pod root"
    );
    every_entry_is_a_bare_target_name(&certs);
    assert_eq!(
        entry_names(&certs),
        ["00303f28", "placeholder"],
        "the aborted write added an entry to session-certs/"
    );

    // And C22's boot sweep takes the leftover away, so the pod root does not
    // collect one per crash.
    let removed = tokio::time::timeout(CEILING, sweep_temporaries(pod.path().to_path_buf()))
        .await
        .expect("the sweep hung")
        .expect("the sweep failed");
    assert_eq!(
        removed, 1,
        "the sweep did not remove the leftover temporary"
    );
    assert_eq!(entry_names(pod.path()), ["session-certs"]);
}

/// A finished session-certificate write leaves the certificate and nothing
/// else, in either directory.
#[tokio::test]
async fn a_session_certificate_write_lands_under_its_own_name() {
    let pod = TempDir::new("session-certs-ok");
    let certs = pod.path().join("session-certs");
    fs::create_dir(&certs).expect("could not create session-certs/");
    fs::write(certs.join("placeholder"), b"").expect("could not seed the placeholder");

    let target = certs.join("00303f28");
    tokio::time::timeout(
        CEILING,
        wirepod_core::persist::write_atomic_with_temporary_in(
            &target,
            PEM_HEAD.to_vec(),
            0o777,
            TemporaryIn::ParentDirectory,
        ),
    )
    .await
    .expect("the write hung")
    .expect("the write failed");

    assert_eq!(
        fs::read(&target).expect("the certificate is missing"),
        PEM_HEAD
    );
    every_entry_is_a_bare_target_name(&certs);
    assert_eq!(entry_names(&certs), ["00303f28", "placeholder"]);
    assert_eq!(
        entry_names(pod.path()),
        ["session-certs"],
        "a temporary survived in the pod root"
    );
}

/// The boot sweep removes what this module names and leaves everything else,
/// because it runs over a directory an operator also keeps files in.
#[tokio::test]
async fn the_boot_sweep_removes_temporaries_and_nothing_else() {
    let directory = TempDir::new("sweep");
    let root = directory.path();

    fs::write(root.join("jdocs.json"), b"state").expect("could not seed the state file");
    fs::write(root.join("jdocs.json.0123456789abcdef.tmp"), b"leftover")
        .expect("could not seed a leftover");
    fs::write(
        root.join("botSdkInfo.json.fedcba9876543210.tmp"),
        b"leftover",
    )
    .expect("could not seed a second leftover");
    // Not this module's: the suffix is not sixteen hex digits.
    fs::write(root.join("notes.tmp"), b"an operator's own")
        .expect("could not seed the operator's file");
    fs::write(root.join("jdocs.json.0123456789abcde.tmp"), b"nearly")
        .expect("could not seed the near miss");
    // A directory whose name would match, which the sweep must not descend into
    // or remove.
    fs::create_dir(root.join("certs.0123456789abcdef.tmp")).expect("could not seed the directory");

    let removed = tokio::time::timeout(CEILING, sweep_temporaries(root.to_path_buf()))
        .await
        .expect("the sweep hung")
        .expect("the sweep failed");

    assert_eq!(removed, 2, "the sweep removed the wrong number of files");
    assert_eq!(
        entry_names(root),
        [
            "certs.0123456789abcdef.tmp",
            "jdocs.json",
            "jdocs.json.0123456789abcde.tmp",
            "notes.tmp",
        ],
        "the sweep took something that was not its own"
    );
    assert_eq!(
        fs::read(root.join("jdocs.json")).expect("the state file is missing"),
        b"state"
    );

    // A second sweep of a directory with nothing left to do is not an error.
    let removed = tokio::time::timeout(CEILING, sweep_temporaries(root.to_path_buf()))
        .await
        .expect("the second sweep hung")
        .expect("the second sweep failed");
    assert_eq!(removed, 0);
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
