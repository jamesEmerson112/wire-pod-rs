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
//! old file exactly as it was. That is deviation 29, and it is the reason the
//! stores may write once where Go writes twice.
//!
//! The whole sequence runs inside `spawn_blocking`, because these are blocking
//! file operations on a runtime whose worker threads are also carrying gRPC
//! streams.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes `contents` to `path`, replacing whatever is there.
///
/// `mode` is the Unix permission bit set Go passes to `os.WriteFile` at the
/// call site being ported, and it is a parameter rather than a constant because
/// Go's writers of one file disagree about it: the bot-info file is `0644` from
/// four call sites (`jdocs/server.go:52`, `:77`, `token/token.go:95`,
/// `jdocs/botInfoStorer.go:152`) and `0777` from the pinger
/// (`sdkapp/jdocspinger.go:250`), and a session certificate is `0777` from
/// `jdocs/botInfoStorer.go:99` and `0755` from `jdocs/server.go:123`. The
/// caller passes the mode of the Go line it reproduces. It is applied to the
/// new file on Unix and ignored on Windows, which has no such bits.
///
/// One difference from `os.WriteFile` follows from the rename: Go passes the
/// mode to `open`, so it applies only when the file is created and an existing
/// file keeps the mode it already had, whereas the file that arrives here is
/// always newly created and always carries `mode` masked by the process umask.
///
/// The error is the underlying [`io::Error`] with no path attached, so a caller
/// that logs it should name the file. Go discards these errors entirely; the
/// port returns them so a full disk is visible.
pub async fn write_atomic(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
) -> io::Result<()> {
    let path = path.into();
    let contents = contents.into();
    tokio::task::spawn_blocking(move || write_blocking(&path, &contents, mode))
        .await
        .map_err(io::Error::other)?
}

/// The blocking half: create, write, flush, rename, and clean up after a
/// failure.
fn write_blocking(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let temporary = temporary_path(path)?;
    match write_then_rename(&temporary, path, contents, mode) {
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
) -> io::Result<()> {
    let mut file = create_new(temporary, mode)?;
    file.write_all(contents)?;
    // `File`'s `flush` is a no-op, so it is `sync_all` that makes the write
    // durable. Without it the rename can be recorded ahead of the bytes and a
    // crash leaves a file that is present, named correctly and empty, which is
    // the one outcome this module exists to prevent.
    file.flush()?;
    file.sync_all()?;
    // Dropped before the rename: Windows will not move a file whose handle is
    // still open without delete sharing.
    drop(file);
    rename_over(temporary, target)
}

/// A name for the temporary file, beside the target so that the rename stays
/// within one filesystem and is therefore a rename rather than a copy.
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
/// no permission bits for `mode` to carry, and Go's own `os.WriteFile` ignores
/// it there for the same reason.
#[cfg(not(unix))]
fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    let _ = mode;
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Renames the temporary over the target.
#[cfg(not(windows))]
fn rename_over(temporary: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temporary, target)
}

/// Renames the temporary over the target, retrying the two Windows errors that
/// mean somebody else is holding the target for a moment.
///
/// `fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, so replacing
/// an existing file is one operation and needs no delete-then-rename dance and
/// no crate. What it cannot do is replace a file another handle is holding
/// without delete sharing, which on Windows is usually a virus scanner or a
/// search indexer that opened the state file moments after it changed. Those
/// holds clear on their own, so a few bounded retries turn a lost write into a
/// slightly slower one. Anything else, including a target that is a directory,
/// is returned on the first attempt.
///
/// The backoff is a constant here rather than a `Timings` field because it is a
/// blocking sleep inside the `spawn_blocking` closure and nothing awaits it;
/// the rule that durations are injected covers the async paths a test has to
/// drive.
#[cfg(windows)]
fn rename_over(temporary: &Path, target: &Path) -> io::Result<()> {
    use std::time::Duration;

    /// `ERROR_ACCESS_DENIED`, which is also what a target that is a directory
    /// answers with, hence the bounded budget.
    const ACCESS_DENIED: i32 = 5;
    /// `ERROR_SHARING_VIOLATION`.
    const SHARING_VIOLATION: i32 = 32;
    /// Four attempts over fourteen milliseconds in total.
    const ATTEMPTS: u32 = 4;
    const FIRST_BACKOFF: Duration = Duration::from_millis(2);

    let mut remaining = ATTEMPTS;
    let mut backoff = FIRST_BACKOFF;
    loop {
        remaining -= 1;
        match fs::rename(temporary, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let transient = matches!(
                    error.raw_os_error(),
                    Some(ACCESS_DENIED | SHARING_VIOLATION)
                );
                if remaining == 0 || !transient {
                    return Err(error);
                }
                std::thread::sleep(backoff);
                backoff *= 2;
            }
        }
    }
}
