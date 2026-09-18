//! Replacing a state file without ever leaving a half-written one behind.
//!
//! Every Go write of a state file is `json.Marshal` followed by
//! `os.WriteFile` (`vars.go:317` for the jdocs file, `jdocs/server.go:52` and
//! `:77`, `token/token.go:95`, `jdocs/botInfoStorer.go:152` and
//! `sdkapp/jdocspinger.go:250` for the bot-info file, `config.go:64`, `:99` and
//! `:155` for the API config). `os.WriteFile` truncates the target and writes
//! into it, so a robot that authenticates while the process dies, or two
//! goroutines writing the same file at once, can leave a truncated or
//! interleaved file on disk. The bot-info file has exactly that shape: five
//! writers, one of them a bare `go os.WriteFile` with no lock at all.
//!
//! [`write_atomic`] replaces the target instead. It writes a temporary file
//! beside it, flushes it to the disk, and renames the temporary over the
//! target, which is a single directory operation: a reader sees either the old
//! file or the new one, never a partial one, and a failure anywhere leaves the
//! old file exactly as it was. That is the atomic-persistence deviation C23
//! records, and it is the reason the stores may write once where Go writes
//! twice.
//!
//! What one atomic write cannot do on its own is order two of them.
//! [`WriteGate`] is that second half: one state file's writes taken in turn,
//! with the marshal moved inside the turn, so the file always ends up holding
//! the state its store was in rather than whichever bytes were marshalled
//! first.
//!
//! The whole sequence runs inside `spawn_blocking`, because these are blocking
//! file operations on a runtime whose worker threads are also carrying gRPC
//! streams.
//!
//! Citations below that begin `os/` or `syscall/` are the Go standard library
//! rather than wire-pod, and their lines are go1.24.4's, the toolchain
//! installed here; every other citation is under `wire-pod/chipper/pkg`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many times [`rename_over`] tries, and how long it waits after the first
/// failure; each later wait is double the last.
///
/// It is a value rather than a pair of constants because the production budget
/// is shorter than the time it takes to arrange a hold on the target from
/// another thread, so a test that used it would be racing its own setup.
/// `test_support::write_atomic_with_retry_budget` hands a test a wide budget
/// and drives the same loop deterministically.
///
/// The wait is a plain blocking sleep rather than a [`crate::timings`] field
/// because nothing awaits it: it runs inside the `spawn_blocking` closure, and
/// the rule that every waited duration is injected covers the async paths a
/// test has to drive.
// The fields are read only by the Windows retry, and every other platform
// renames once.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryBudget {
    /// Attempts in total, the first one included.
    pub(crate) attempts: u32,
    /// The wait after the first failure.
    pub(crate) first_backoff: Duration,
}

impl RetryBudget {
    /// Four attempts over fourteen milliseconds of waiting in total.
    pub(crate) const PRODUCTION: Self = Self {
        attempts: 4,
        first_backoff: Duration::from_millis(2),
    };
}

/// Writes `contents` to `path`, replacing whatever is there.
///
/// `mode` is the Unix permission bit set Go passes to `os.WriteFile` at the
/// call site being ported, and it is a parameter rather than a constant because
/// Go's writers of one file disagree about it: the bot-info file is `0644` from
/// four call sites (`jdocs/server.go:52`, `:77`, `token/token.go:95`,
/// `jdocs/botInfoStorer.go:152`) and `0777` from the pinger
/// (`sdkapp/jdocspinger.go:250`), and a session certificate is `0777` from
/// `jdocs/botInfoStorer.go:99` and `0755` from `jdocs/server.go:123`. The
/// caller passes the mode of the Go line it reproduces.
///
/// `mode` follows `os.WriteFile` exactly. Go opens with `O_CREATE` and hands
/// `perm` to the open (`os/file.go:849-859`), and `open(2)` applies a mode only
/// when it creates the file, so `mode` reaches a file this call creates, masked
/// by the process umask, and a file that already exists keeps the mode it
/// already had whatever the caller asks for. Because the file that arrives here
/// is always newly created, keeping an existing mode takes a deliberate step,
/// which is what `carry_over_existing_mode` is. On Windows nothing happens:
/// the only thing Go's own `OpenFile` reads out of `perm` there is the owner
/// write bit, which it turns into `FILE_ATTRIBUTE_READONLY`
/// (`syscall/syscall_windows.go:380-382`), and every mode in this list sets it.
///
/// The error is the underlying [`io::Error`] with no path attached, so a caller
/// that logs it should name the file. Go discards these errors entirely; the
/// port returns them so a full disk is visible.
pub async fn write_atomic(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
) -> io::Result<()> {
    write_atomic_with_budget(path, contents, mode, RetryBudget::PRODUCTION).await
}

/// [`write_atomic`] with the rename retry's budget chosen by the caller, which
/// only a test does.
pub(crate) async fn write_atomic_with_budget(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    budget: RetryBudget,
) -> io::Result<()> {
    let path = path.into();
    let contents = contents.into();
    tokio::task::spawn_blocking(move || write_blocking(&path, &contents, mode, budget))
        .await
        .map_err(io::Error::other)?
}

/// One state file, with every write of it taken in turn.
///
/// [`write_atomic`] makes a single write indivisible, but two of them can still
/// finish in either order, and a store that marshals its list and only then
/// awaits the write can have the two orders disagree. A large mutation and a
/// small one issued at nearly the same instant land their renames in the order
/// the writes finish, which is the order of their sizes, while their bytes were
/// decided in the order they marshalled; the file is then left holding a state
/// the store itself has moved past, and stays that way until something writes
/// again. Go's jdocs file has exactly that shape: the pinger's ticker calls
/// `AddJdoc` from its own goroutine (`sdkapp/jdocspinger.go:126`) while a
/// robot's `WriteDoc` (`jdocs/server.go:40`) or the `DeleteData` its `ReadDocs`
/// reaches (`jdocs/server.go:83`) runs on a gRPC handler.
///
/// Go converges anyway wherever a caller writes twice, which two of its four
/// `AddJdoc` callers do: `WriteDoc` and `WriteTokenHash` each call `WriteJdocs`
/// again straight after (`jdocs/server.go:40-41`, `token/token.go:125-126`),
/// and that second write re-marshals the list at a later instant. A gate gives
/// every call site what only those two have: [`WriteGate::write`] takes the
/// turn first and marshals inside it, so whichever write takes the gate last
/// marshals after every mutation that has already asked to be written, and the
/// writes themselves finish in the order they took the gate.
///
/// The turn is a [`tokio::sync::Mutex`] and its guard is deliberately held
/// across the write's `.await`, which is why it is a `tokio` one and not a
/// [`std::sync::Mutex`], the same reason the per-serial connect lock in
/// [`crate::robot::registry`] is. The lists the stores keep stay behind
/// [`std::sync::Mutex`], their guards are taken and dropped inside the
/// marshalling closure, and the crate-level `deny(clippy::await_holding_lock)`
/// keeps it that way.
///
/// One gate covers one file. Two gates over the same path would order nothing,
/// so a file with more than one writer hands them all the same gate.
#[derive(Debug)]
pub struct WriteGate {
    /// The file, as the caller spelled it. A [`String`] because that is what
    /// [`crate::paths::DataDir`] hands out: Go builds these paths by
    /// concatenation and they reach the disk with the separators Go put in
    /// them.
    path: String,
    /// The mode of the `os.WriteFile` call site being reproduced, as
    /// [`write_atomic`] takes it.
    mode: u32,
    /// Whose turn it is. The unit is the point: the gate orders writes and
    /// guards nothing.
    turn: tokio::sync::Mutex<()>,
}

impl WriteGate {
    /// The gate for `path`, whose writes carry `mode`.
    pub fn new(path: impl Into<String>, mode: u32) -> Self {
        Self {
            path: path.into(),
            mode,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The file this gate writes, as the caller spelled it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Takes the gate, calls `marshal`, and replaces the file with what it
    /// produced.
    ///
    /// `marshal` runs inside the turn and nowhere else, which is what makes the
    /// bytes reaching the disk no older than the moment this write began. It is
    /// called exactly once, synchronously, so a closure that reads a
    /// [`std::sync::Mutex`] may take its guard and drop it inside the closure
    /// body without that guard ever meeting an `.await`.
    ///
    /// # Errors
    ///
    /// Whatever [`write_atomic`] reports. A failure leaves the file exactly as
    /// it was and releases the gate, so a later write is unaffected.
    pub async fn write(&self, marshal: impl FnOnce() -> Vec<u8>) -> io::Result<()> {
        let _turn = self.turn.lock().await;
        let contents = marshal();
        write_atomic(PathBuf::from(&self.path), contents, self.mode).await
    }
}

/// The blocking half: create, write, flush, rename, and clean up after a
/// failure.
fn write_blocking(path: &Path, contents: &[u8], mode: u32, budget: RetryBudget) -> io::Result<()> {
    let temporary = temporary_path(path)?;
    match write_then_rename(&temporary, path, contents, mode, budget) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Best effort, and deliberately not reported: the caller's error is
            // the one that explains the failure, and a temporary that cannot be
            // removed is not worth masking it with. The removal only has
            // something to do when the failure came after the file was created,
            // which in practice means the rename was refused.
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Fills the temporary file and moves it into place.
fn write_then_rename(
    temporary: &Path,
    target: &Path,
    contents: &[u8],
    mode: u32,
    budget: RetryBudget,
) -> io::Result<()> {
    let mut file = create_new(temporary, mode)?;
    file.write_all(contents)?;
    // `File`'s `flush` is a no-op, so it is `sync_all` that makes the write
    // durable. Without it the rename can be recorded ahead of the bytes and a
    // crash leaves a file that is present, named correctly and empty, which is
    // the one outcome this module exists to prevent.
    file.flush()?;
    file.sync_all()?;
    // Dropped before the rename so the bytes are committed and so the sequence
    // does not quietly rest on std's Windows share-mode defaults, which do
    // grant `FILE_SHARE_DELETE` and so would let the rename through anyway.
    drop(file);
    carry_over_existing_mode(temporary, target)?;
    rename_over(temporary, target, budget)
}

/// A name for the temporary file, beside the target so that the rename stays
/// within one filesystem and is therefore a rename rather than a copy.
///
/// Beside it, and not in the system temporary directory, is the load-bearing
/// part on the target platform: `%APPDATA%` and `%TEMP%` sit on different
/// volumes whenever `TEMP` is redirected to another drive, and a cross-volume
/// `fs::rename` fails outright rather than copying.
///
/// The random suffix is what lets two writers of the same file run at once:
/// each fills its own temporary and the loser of the rename race is simply
/// overwritten, so the file on disk is always one writer's bytes in full.
fn temporary_path(target: &Path) -> io::Result<PathBuf> {
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the target path does not name a file",
        )
    })?;
    // `parent` is `Some("")` for a bare file name, and joining onto the empty
    // path gives the name back, which is the same directory the target is in.
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the target path has no directory to write into",
        )
    })?;
    let mut suffix = [0u8; 8];
    // Through `Display` rather than through `io::Error::other`, because
    // `getrandom::Error` only implements `std::error::Error` when that crate's
    // `std` feature is on, and this crate does not ask for it. Relying on
    // another workspace member to turn it on would make this file compile only
    // in a wide enough build.
    getrandom::getrandom(&mut suffix)
        .map_err(|error| io::Error::other(format!("could not name a temporary file: {error}")))?;
    let mut name = file_name.to_os_string();
    name.push(format!(".{:016x}.tmp", u64::from_ne_bytes(suffix)));
    Ok(parent.join(name))
}

/// Creates the temporary file, refusing to touch an existing one, with Go's
/// mode on the platforms that have one.
#[cfg(unix)]
fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
}

/// Creates the temporary file, refusing to touch an existing one. Windows has
/// no permission bit set for `mode` to carry, so it is dropped here the way
/// Go's `OpenFile` drops all of `perm` but its owner write bit
/// (`syscall/syscall_windows.go:380-382`).
#[cfg(not(unix))]
fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    let _ = mode;
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Puts the target's own permissions on the temporary when the target already
/// exists, which is the half of `os.WriteFile`'s mode handling the rename would
/// otherwise lose.
///
/// Go hands the mode to an `O_CREATE` open (`os/file.go:849-859`) and `open(2)`
/// applies a mode only when it creates the file, so in Go an existing file
/// keeps its mode however the call site spells `perm`. Here the file being
/// renamed into place is always new, so without this step the pinger's `0777`
/// (`sdkapp/jdocspinger.go:250`) would move a `botSdkInfo.json` that one of the
/// four `0644` writers created off `0644`, and the file mode is on-disk state
/// the parity rule covers.
///
/// It is `chmod` rather than the open's mode because `open` masks with the
/// umask and `chmod` does not: the bits being carried over are ones the file
/// already has, not ones being asked for.
///
/// A target that cannot be stat'ed, or that is not a regular file, is left to
/// the rename to report: a missing target is the ordinary create case, and a
/// directory in the target's place is a failure whose error should be the
/// rename's.
#[cfg(unix)]
fn carry_over_existing_mode(temporary: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Ok(existing) = fs::metadata(target) else {
        return Ok(());
    };
    if !existing.is_file() {
        return Ok(());
    }
    fs::set_permissions(
        temporary,
        fs::Permissions::from_mode(existing.permissions().mode() & 0o7777),
    )
}

/// Nothing to carry over: Windows has no permission bit set, and the one thing
/// Go's `OpenFile` reads out of `perm` there, the owner write bit that becomes
/// `FILE_ATTRIBUTE_READONLY` (`syscall/syscall_windows.go:380-382`), is set by
/// every mode any call site in this port passes.
#[cfg(not(unix))]
fn carry_over_existing_mode(_temporary: &Path, _target: &Path) -> io::Result<()> {
    Ok(())
}

/// Renames the temporary over the target. Nothing outside Windows holds a file
/// in a way that refuses a rename, so the budget goes unread.
#[cfg(not(windows))]
fn rename_over(temporary: &Path, target: &Path, _budget: RetryBudget) -> io::Result<()> {
    fs::rename(temporary, target)
}

/// Renames the temporary over the target, retrying the Windows errors that mean
/// somebody else is holding the target for a moment.
///
/// `fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, so replacing
/// an existing file is one operation and needs no delete-then-rename dance and
/// no crate. What it cannot do is replace a file another handle is holding
/// without delete sharing, which on Windows is usually a virus scanner or a
/// search indexer that opened the state file moments after it changed. Those
/// holds clear on their own, so a few bounded retries turn a lost write into a
/// slightly slower one. What [`is_transient`] rejects is returned on the first
/// attempt.
#[cfg(windows)]
fn rename_over(temporary: &Path, target: &Path, budget: RetryBudget) -> io::Result<()> {
    // A budget of nothing would still have to try once.
    let mut remaining = budget.attempts.max(1);
    let mut backoff = budget.first_backoff;
    loop {
        remaining -= 1;
        match fs::rename(temporary, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if remaining == 0 || !is_transient(&error) {
                    return Err(error);
                }
                std::thread::sleep(backoff);
                backoff *= 2;
            }
        }
    }
}

/// Whether a failed rename is worth trying again.
///
/// `ERROR_ACCESS_DENIED` is what `MoveFileExW` answers both when another handle
/// holds the target without delete sharing and when the target is a directory,
/// so the error alone cannot tell the two apart and both spend the budget; the
/// budget is small precisely because the hopeless case is in it.
/// `ERROR_SHARING_VIOLATION` is the other code a hold can surface as. Anything
/// else, and any error carrying no Windows code at all, is hopeless in a way
/// waiting cannot change.
#[cfg(windows)]
fn is_transient(error: &io::Error) -> bool {
    /// `ERROR_ACCESS_DENIED`.
    const ACCESS_DENIED: i32 = 5;
    /// `ERROR_SHARING_VIOLATION`.
    const SHARING_VIOLATION: i32 = 32;

    matches!(
        error.raw_os_error(),
        Some(ACCESS_DENIED | SHARING_VIOLATION)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is a tuning knob rather than behaviour, and what pins it is a
    /// second statement of the number rather than a measurement. No elapsed
    /// time can tell four attempts from one: creating, filling and syncing the
    /// temporary costs more on a busy machine than the whole budget spends
    /// waiting. Widening it, which the side-by-side run may well ask for, is
    /// then an edit here as well as above, which is the point.
    #[cfg(windows)]
    #[test]
    fn the_production_budget_is_four_attempts_over_fourteen_milliseconds() {
        let budget = RetryBudget::PRODUCTION;

        assert_eq!(budget.attempts, 4, "the production attempt count moved");
        let waited: Duration = (0..budget.attempts - 1)
            .map(|step| budget.first_backoff * 2u32.pow(step))
            .sum();
        assert_eq!(
            waited,
            Duration::from_millis(14),
            "the production budget now waits {waited:?} in total"
        );
    }

    /// The temporary has to be a sibling of the target. A rename is only atomic,
    /// and only a rename rather than a copy, within one filesystem, and on the
    /// target platform `%APPDATA%` and the system temporary directory are on
    /// different volumes whenever `TEMP` is redirected to another drive.
    #[test]
    fn the_temporary_sits_in_the_targets_own_directory() {
        let target = Path::new("jdocs").join("jdocs.json");

        let temporary = temporary_path(&target).expect("naming a temporary failed");

        assert_eq!(
            temporary.parent(),
            Some(Path::new("jdocs")),
            "the temporary left the target's own directory"
        );
        let name = temporary
            .file_name()
            .expect("the temporary has no file name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("jdocs.json."),
            "the temporary {name} is not named after the target"
        );
        assert!(name.ends_with(".tmp"), "the temporary {name} is not a .tmp");
        assert_ne!(
            temporary,
            temporary_path(&target).expect("naming a second temporary failed"),
            "two writers of one file would collide on the temporary's name"
        );
    }

    /// A target with no directory in it resolves against the working directory,
    /// which is where the source layout's own `jdocs/jdocs.json` lives.
    #[test]
    fn a_target_with_no_directory_stays_in_the_working_directory() {
        let temporary = temporary_path(Path::new("jdocs.json")).expect("naming a temporary failed");

        assert_eq!(
            temporary.parent(),
            Some(Path::new("")),
            "the temporary left the working directory"
        );
    }

    /// The classification behind the retry, which no test can reach through
    /// [`write_atomic`]: the one failure a test can arrange on Windows, a
    /// directory in the target's place, answers `ERROR_ACCESS_DENIED` and is
    /// therefore in the retried set itself.
    #[cfg(windows)]
    #[test]
    fn only_the_two_codes_a_hold_produces_are_retried() {
        for code in [5, 32] {
            assert!(
                is_transient(&io::Error::from_raw_os_error(code)),
                "Windows error {code} is what a hold looks like and must be retried"
            );
        }
        // `ERROR_FILE_NOT_FOUND`, `ERROR_PATH_NOT_FOUND`, `ERROR_NOT_SAME_DEVICE`,
        // `ERROR_INVALID_PARAMETER` and `ERROR_INVALID_NAME`.
        for code in [2, 3, 17, 87, 123] {
            assert!(
                !is_transient(&io::Error::from_raw_os_error(code)),
                "Windows error {code} cannot clear on its own and must not be retried"
            );
        }
        assert!(
            !is_transient(&io::Error::other("an error with no Windows code at all")),
            "an error with no Windows code must not be retried"
        );
    }
}
