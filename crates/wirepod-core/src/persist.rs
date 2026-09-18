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
//! The temporary is the one thing this module adds to the directory Go's
//! writers work in, and in one directory that is visible to Go.
//! `ReadSessionCerts` lists `session-certs/` at boot (`vars.go:371`, called
//! from `vars.go:256`) and treats every entry whose name is not the literal
//! `placeholder` as a robot's ESN (`vars.go:377-380`). It then reads the entry
//! and dereferences `pem.Decode`'s result without checking it for nil
//! (`vars.go:387-388`), so a temporary left in that directory by a process that
//! died between creating it and renaming it makes a rolled-back Go server panic
//! at boot, and a filled one registers a bogus ESN. [`TemporaryIn`] is the
//! answer: a writer of a file under `session-certs/` asks for
//! [`TemporaryIn::ParentDirectory`] and its temporary lands in the pod root,
//! which is on the same volume, which Go never enumerates, and which is not a
//! name `ReadSessionCerts` can reach. The session-certificate writer C9 brings
//! must use it. [`sweep_temporaries`] is the other half, for the boot path C22:
//! it removes the temporaries an earlier crash left in a state directory.
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

/// How many hex digits [`temporary_path`] puts between the target's name and
/// the `.tmp` suffix, and therefore how many [`is_temporary_name`] demands.
///
/// The two have to agree: the sweep exists to remove this module's leavings and
/// nothing else, so the pattern it recognises is the pattern the namer writes.
const TEMPORARY_HEX_LEN: usize = 16;

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

    /// The waits this budget spends, in order, one per attempt after the first.
    ///
    /// [`rename_over`] sleeps for exactly what this yields and ends when it
    /// runs out, so the loop and the sequence a test asserts are the same code
    /// rather than two statements of the same formula. A budget of one attempt,
    /// or of none, yields nothing and the loop tries once.
    ///
    /// The doubling saturates rather than panicking: [`Duration`]'s
    /// multiplication is checked, and a budget wide enough to overflow it is a
    /// mistake that should stop at the first sleep rather than in a panic
    /// inside `spawn_blocking`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn waits(&self) -> impl Iterator<Item = Duration> + use<> {
        let mut backoff = self.first_backoff;
        (0..self.attempts.saturating_sub(1)).map(move |_| {
            let wait = backoff;
            backoff = backoff.checked_mul(2).unwrap_or(Duration::MAX);
            wait
        })
    }
}

/// Where a write puts the temporary file it renames over the target.
///
/// The temporary is a real directory entry for as long as the write lasts, and
/// for good after a crash, so the answer depends on who else reads the
/// directory. The pod root is read by nobody in Go; `session-certs/` is read by
/// `ReadSessionCerts`, which treats every name in it as an ESN and panics on a
/// file that is not a PEM certificate (`vars.go:377-388`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TemporaryIn {
    /// Beside the target, which is where every state file at the pod root wants
    /// it: the rename is then within one directory, and no Go reader lists that
    /// directory.
    #[default]
    TargetDirectory,
    /// One directory above the target's own, which for a file under
    /// `session-certs/` is the pod root.
    ///
    /// Still the same volume, so the rename is still a rename rather than a
    /// copy, and still not the system temporary directory, which on the target
    /// platform is a different volume whenever `TEMP` is redirected. A `.tmp`
    /// subdirectory of `session-certs/` would not do instead: `ReadSessionCerts`
    /// reads every entry's name as an ESN without asking whether it is a
    /// directory (`vars.go:376-382`), so the subdirectory itself would become a
    /// bogus robot.
    ParentDirectory,
}

/// Writes `contents` to `path`, replacing whatever is there, with the temporary
/// beside it.
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
/// A writer of a file under `session-certs/` wants
/// [`write_atomic_with_temporary_in`] instead, for the reason [`TemporaryIn`]
/// gives.
///
/// The error is the underlying [`io::Error`] with no path attached, so a caller
/// that logs it should name the file. Go discards these errors entirely; the
/// port returns them so a full disk is visible.
pub async fn write_atomic(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
) -> io::Result<()> {
    write_atomic_with_temporary_in(path, contents, mode, TemporaryIn::TargetDirectory).await
}

/// [`write_atomic`] with the temporary's directory chosen by the caller.
///
/// The only caller that needs anything but [`TemporaryIn::TargetDirectory`] is
/// the session-certificate writer C9 brings, which must pass
/// [`TemporaryIn::ParentDirectory`] so that nothing this module creates is ever
/// a directory entry `ReadSessionCerts` can read as an ESN (`vars.go:377-388`).
///
/// # Errors
///
/// Whatever [`write_atomic`] reports, and additionally
/// [`io::ErrorKind::InvalidInput`] when the requested directory does not exist
/// as a path, which for [`TemporaryIn::ParentDirectory`] means a target with
/// nothing above its own directory.
pub async fn write_atomic_with_temporary_in(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    temporary_in: TemporaryIn,
) -> io::Result<()> {
    write_atomic_with_budget(path, contents, mode, temporary_in, RetryBudget::PRODUCTION)
        .await
        .map(|_attempts| ())
}

/// [`write_atomic`] with the rename retry's budget chosen by the caller, which
/// only a test does, answering how many rename attempts it took.
///
/// The count is what lets the Windows hold test assert that it exercised the
/// retry rather than passing because the hold had already cleared. Every
/// platform counts; only Windows can ever answer more than one.
pub(crate) async fn write_atomic_with_budget(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    temporary_in: TemporaryIn,
    budget: RetryBudget,
) -> io::Result<u32> {
    let path = path.into();
    let contents = contents.into();
    tokio::task::spawn_blocking(move || {
        write_blocking(&path, &contents, mode, temporary_in, budget)
    })
    .await
    .map_err(io::Error::other)?
}

/// Creates and fills the temporary for `path` and stops there, leaving it
/// exactly where a process that died before the rename would leave it, and
/// answering where that is.
///
/// This is the seam the session-certificate test drives: the property being
/// pinned is what a crash leaves in `session-certs/`, and nothing that finishes
/// a write can show it. Nothing in production calls it, which is why it is
/// compiled only into a `test-util` build, and unlike every failure path in
/// [`write_blocking`] it deliberately does not remove the temporary.
#[cfg(feature = "test-util")]
pub(crate) async fn fill_temporary_and_stop(
    path: impl Into<PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    temporary_in: TemporaryIn,
) -> io::Result<PathBuf> {
    let path = path.into();
    let contents = contents.into();
    tokio::task::spawn_blocking(move || {
        let temporary = temporary_path(&path, temporary_in)?;
        fill_temporary(&mut RealWrite::default(), &temporary, &contents, mode)?;
        Ok(temporary)
    })
    .await
    .map_err(io::Error::other)?
}

/// Removes the temporaries an earlier crash left in `dir`, answering how many
/// went.
///
/// A write that is interrupted between creating its temporary and renaming it
/// leaves that temporary behind, and nothing removes it afterwards: the failure
/// paths in [`write_blocking`] only cover a write whose process lived to see
/// the failure. The boot path C22 sweeps each state directory once, before
/// anything reads it, which is why a concurrent write is not a concern here and
/// why this must not be called while the server is serving.
///
/// Only names [`is_temporary_name`] recognises are touched, and only regular
/// files, so an operator's own `notes.tmp` and any directory are left alone. An
/// entry that disappears between the listing and the removal is not an error:
/// two sweeps of one directory is a harmless thing to do.
///
/// # Errors
///
/// A directory that cannot be listed, or a temporary that cannot be removed for
/// any reason but having gone already.
pub async fn sweep_temporaries(dir: impl Into<PathBuf>) -> io::Result<usize> {
    let dir = dir.into();
    tokio::task::spawn_blocking(move || sweep_blocking(&dir))
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
/// so a file with more than one writer hands them all the same gate. That is
/// why `apiConfig.json`'s three writers take a `&WriteGate` rather than a
/// directory: they are three writers of one file, and the gate is the thing
/// C13 holds in `AppState` so that all three share it.
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
    /// Where this file's temporaries go, which is not the target's own
    /// directory for a file under `session-certs/`.
    temporary_in: TemporaryIn,
    /// Whose turn it is. The unit is the point: the gate orders writes and
    /// guards nothing.
    turn: tokio::sync::Mutex<()>,
}

impl WriteGate {
    /// The gate for `path`, whose writes carry `mode` and put their temporary
    /// beside the target.
    pub fn new(path: impl Into<String>, mode: u32) -> Self {
        Self::with_temporary_in(path, mode, TemporaryIn::TargetDirectory)
    }

    /// The gate for `path` with the temporary's directory chosen, which the
    /// session-certificate writer C9 brings needs and nothing else does.
    pub fn with_temporary_in(
        path: impl Into<String>,
        mode: u32,
        temporary_in: TemporaryIn,
    ) -> Self {
        Self {
            path: path.into(),
            mode,
            temporary_in,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The file this gate writes, as the caller spelled it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Where this gate's writes put their temporary.
    ///
    /// The placement is only otherwise observable by catching a write in
    /// flight, which no test can do without racing it, so the writer of a file
    /// under `session-certs/` is checked against this instead
    /// (`crates/wirepod-core/src/store/session_certs.rs`).
    pub fn temporary_in(&self) -> TemporaryIn {
        self.temporary_in
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
        write_atomic_with_temporary_in(
            PathBuf::from(&self.path),
            contents,
            self.mode,
            self.temporary_in,
        )
        .await
    }
}

/// The blocking half: create, write, flush, rename, and clean up after a
/// failure. Answers how many rename attempts it took.
fn write_blocking(
    path: &Path,
    contents: &[u8],
    mode: u32,
    temporary_in: TemporaryIn,
    budget: RetryBudget,
) -> io::Result<u32> {
    let temporary = temporary_path(path, temporary_in)?;
    match write_then_rename(
        &mut RealWrite::default(),
        &temporary,
        path,
        contents,
        mode,
        budget,
    ) {
        Ok(attempts) => Ok(attempts),
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

/// The file operations one atomic write makes, in the order it makes them.
///
/// The order is the property, and it is the one property no in-process
/// observation can check: a write that skipped `sync_all` produces exactly the
/// bytes a write that called it produces, and only a power cut tells them
/// apart. The seam is here so a recording implementation can, and it is
/// deliberately nothing more than the six calls [`write_then_rename`] makes.
trait WriteSteps {
    /// Creates the temporary, refusing an existing one.
    fn create_new(&mut self, path: &Path, mode: u32) -> io::Result<()>;
    /// Writes the whole body into it.
    fn write_all(&mut self, contents: &[u8]) -> io::Result<()>;
    /// Flushes whatever buffering sits above the file descriptor.
    fn flush(&mut self) -> io::Result<()>;
    /// Commits the bytes to the disk.
    fn sync_all(&mut self) -> io::Result<()>;
    /// Releases the handle before the rename.
    fn close(&mut self);
    /// Moves the temporary over the target, answering the attempts it took.
    fn rename(&mut self, temporary: &Path, target: &Path, budget: RetryBudget) -> io::Result<u32>;
}

/// [`WriteSteps`] against the real filesystem.
#[derive(Debug, Default)]
struct RealWrite {
    /// The temporary, from `create_new` until `close`.
    file: Option<File>,
}

impl RealWrite {
    /// The handle the middle three steps work through.
    ///
    /// [`write_then_rename`] calls them in order, so this is always `Some`
    /// there; the error covers a future caller that reorders them and says
    /// which rule it broke.
    fn opened(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("the temporary was written before it was created"))
    }
}

impl WriteSteps for RealWrite {
    fn create_new(&mut self, path: &Path, mode: u32) -> io::Result<()> {
        self.file = Some(create_new(path, mode)?);
        Ok(())
    }

    fn write_all(&mut self, contents: &[u8]) -> io::Result<()> {
        self.opened()?.write_all(contents)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.opened()?.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.opened()?.sync_all()
    }

    fn close(&mut self) {
        self.file = None;
    }

    fn rename(&mut self, temporary: &Path, target: &Path, budget: RetryBudget) -> io::Result<u32> {
        rename_over(temporary, target, budget)
    }
}

/// Fills the temporary file: create, write, flush, sync, close.
///
/// `File`'s `flush` is a no-op, so it is `sync_all` that makes the write
/// durable. Without it the rename can be recorded ahead of the bytes and a
/// crash leaves a file that is present, named correctly and empty, which is the
/// one outcome this module exists to prevent.
///
/// The handle is released before the caller renames, so the bytes are committed
/// and so the sequence does not quietly rest on std's Windows share-mode
/// defaults, which do grant `FILE_SHARE_DELETE` and so would let the rename
/// through anyway.
fn fill_temporary<S: WriteSteps>(
    steps: &mut S,
    temporary: &Path,
    contents: &[u8],
    mode: u32,
) -> io::Result<()> {
    steps.create_new(temporary, mode)?;
    steps.write_all(contents)?;
    steps.flush()?;
    steps.sync_all()?;
    steps.close();
    Ok(())
}

/// Fills the temporary file and moves it into place, answering the rename
/// attempts it took.
fn write_then_rename<S: WriteSteps>(
    steps: &mut S,
    temporary: &Path,
    target: &Path,
    contents: &[u8],
    mode: u32,
    budget: RetryBudget,
) -> io::Result<u32> {
    fill_temporary(steps, temporary, contents, mode)?;
    carry_over_existing_mode(temporary, target)?;
    steps.rename(temporary, target, budget)
}

/// A name for the temporary file, in the directory `temporary_in` chooses, so
/// that the rename stays within one filesystem and is therefore a rename rather
/// than a copy.
///
/// Within one filesystem, and not in the system temporary directory, is the
/// load-bearing part on the target platform: `%APPDATA%` and `%TEMP%` sit on
/// different volumes whenever `TEMP` is redirected to another drive, and a
/// cross-volume `fs::rename` fails outright rather than copying. Both
/// [`TemporaryIn`] answers stay inside the pod directory tree and so inside one
/// volume.
///
/// The random suffix is what lets two writers of the same file run at once:
/// each fills its own temporary and the loser of the rename race is simply
/// overwritten, so the file on disk is always one writer's bytes in full. It is
/// also what [`is_temporary_name`] matches, so its shape is a contract between
/// the namer and the sweep.
fn temporary_path(target: &Path, temporary_in: TemporaryIn) -> io::Result<PathBuf> {
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the target path does not name a file",
        )
    })?;
    // `parent` is `Some("")` for a bare file name, and joining onto the empty
    // path gives the name back, which is the same directory the target is in.
    let own = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the target path has no directory to write into",
        )
    })?;
    let directory = match temporary_in {
        TemporaryIn::TargetDirectory => own,
        TemporaryIn::ParentDirectory => own.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the target's directory has no parent to write the temporary into",
            )
        })?,
    };
    let mut suffix = [0u8; 8];
    // Through `Display` rather than through `io::Error::other`, because
    // `getrandom::Error` only implements `std::error::Error` when that crate's
    // `std` feature is on, and this crate does not ask for it. Relying on
    // another workspace member to turn it on would make this file compile only
    // in a wide enough build.
    getrandom::getrandom(&mut suffix)
        .map_err(|error| io::Error::other(format!("could not name a temporary file: {error}")))?;
    let mut name = file_name.to_os_string();
    name.push(format!(
        ".{:0width$x}.tmp",
        u64::from_ne_bytes(suffix),
        width = TEMPORARY_HEX_LEN
    ));
    Ok(directory.join(name))
}

/// Whether `name` is a name [`temporary_path`] produced: something, a dot,
/// [`TEMPORARY_HEX_LEN`] hex digits, and `.tmp`.
///
/// Deliberately exact. The sweep removes files at boot, and a rule as loose as
/// "ends in `.tmp`" would take an operator's own scratch file with it. Upper
/// case hex is accepted although the namer never writes it, because a reader
/// should be the more forgiving of the two.
fn is_temporary_name(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".tmp") else {
        return false;
    };
    let Some((stem, hex)) = rest.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && hex.len() == TEMPORARY_HEX_LEN
        && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The blocking half of [`sweep_temporaries`].
fn sweep_blocking(dir: &Path) -> io::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        // A name that is not UTF-8 cannot be one this module wrote: every
        // temporary's name is a Go-built path's last component plus ASCII.
        let Some(name) = name.to_str() else { continue };
        if !is_temporary_name(name) || !entry.file_type()?.is_file() {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}

/// Creates the temporary file, refusing to touch an existing one, with Go's
/// mode on the platforms that have one.
///
/// `create_new` rather than `create` is what makes the random suffix mean
/// something: two writers that happened to draw the same name must not end up
/// sharing one file descriptor's worth of bytes, and a stale temporary must not
/// be adopted and half-overwritten.
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
/// in a way that refuses a rename, so the budget goes unread and the one
/// attempt is the only one.
#[cfg(not(windows))]
fn rename_over(temporary: &Path, target: &Path, _budget: RetryBudget) -> io::Result<u32> {
    fs::rename(temporary, target).map(|()| 1)
}

/// Renames the temporary over the target, retrying the Windows errors that mean
/// somebody else is holding the target for a moment, and answering how many
/// attempts it took.
///
/// `fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, so replacing
/// an existing file is one operation and needs no delete-then-rename dance and
/// no crate. What it cannot do is replace a file another handle is holding
/// without delete sharing, which on Windows is usually a virus scanner or a
/// search indexer that opened the state file moments after it changed. Those
/// holds clear on their own, so a few bounded retries turn a lost write into a
/// slightly slower one. What [`is_transient`] rejects is returned on the first
/// attempt.
///
/// The waits come from [`RetryBudget::waits`] and the loop ends when they run
/// out, so the budget's shape lives in one place rather than being restated
/// here.
#[cfg(windows)]
fn rename_over(temporary: &Path, target: &Path, budget: RetryBudget) -> io::Result<u32> {
    let mut waits = budget.waits();
    let mut attempts = 0;
    loop {
        attempts += 1;
        match fs::rename(temporary, target) {
            Ok(()) => return Ok(attempts),
            Err(error) => {
                let Some(wait) = waits.next().filter(|_| is_transient(&error)) else {
                    return Err(error);
                };
                std::thread::sleep(wait);
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

    use std::sync::atomic::{AtomicU64, Ordering};

    /// A directory under the system temporary directory, removed when the test
    /// ends, so nothing here reaches the repository or the live
    /// `%APPDATA%\wire-pod`.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);

            let mut path = std::env::temp_dir();
            path.push(format!(
                "wirepod-persist-unit-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("could not create the temporary directory");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// A [`WriteSteps`] that does nothing but remember what it was asked to do.
    #[derive(Debug, Default)]
    struct RecordedWrite {
        steps: Vec<&'static str>,
        wrote: Vec<u8>,
    }

    impl WriteSteps for RecordedWrite {
        fn create_new(&mut self, _path: &Path, _mode: u32) -> io::Result<()> {
            self.steps.push("create_new");
            Ok(())
        }

        fn write_all(&mut self, contents: &[u8]) -> io::Result<()> {
            self.steps.push("write_all");
            self.wrote.extend_from_slice(contents);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.steps.push("flush");
            Ok(())
        }

        fn sync_all(&mut self) -> io::Result<()> {
            self.steps.push("sync_all");
            Ok(())
        }

        fn close(&mut self) {
            self.steps.push("close");
        }

        fn rename(
            &mut self,
            _temporary: &Path,
            _target: &Path,
            _budget: RetryBudget,
        ) -> io::Result<u32> {
            self.steps.push("rename");
            Ok(1)
        }
    }

    /// The order every atomic write makes its file calls in, which is the one
    /// property no finished write can be inspected for: a write that never
    /// called `sync_all` leaves exactly the bytes a write that did leaves, and
    /// only a power cut separates them. The recorder is what makes the order
    /// checkable, and the `sync_all` between the fill and the rename is why
    /// this module exists.
    #[test]
    fn the_write_creates_fills_syncs_and_closes_before_it_renames() {
        let directory = TempDir::new("order");
        let target = directory.path.join("botSdkInfo.json");
        let temporary = directory.path.join("botSdkInfo.json.0123456789abcdef.tmp");
        let mut recorder = RecordedWrite::default();

        let attempts = write_then_rename(
            &mut recorder,
            &temporary,
            &target,
            b"body",
            0o644,
            RetryBudget::PRODUCTION,
        )
        .expect("the recorded write failed");

        assert_eq!(attempts, 1);
        assert_eq!(
            recorder.steps,
            [
                "create_new",
                "write_all",
                "flush",
                "sync_all",
                "close",
                "rename"
            ],
            "the file steps moved"
        );
        assert_eq!(recorder.wrote, b"body");
    }

    /// The temporary is created with `create_new`, so a name that already
    /// exists is refused rather than adopted and half-overwritten. Nothing a
    /// finished write leaves behind can show this, because the namer never
    /// draws the same suffix twice; the step is called directly instead.
    #[test]
    fn creating_a_temporary_refuses_a_name_that_already_exists() {
        let directory = TempDir::new("createnew");
        let temporary = directory.path.join("jdocs.json.0123456789abcdef.tmp");

        let first = create_new(&temporary, 0o644).expect("the first create failed");
        drop(first);

        let error = create_new(&temporary, 0o644)
            .expect_err("a second create of the same name must be refused");
        assert_eq!(
            error.kind(),
            io::ErrorKind::AlreadyExists,
            "the temporary was opened over an existing file"
        );
    }

    /// The budget is a tuning knob rather than behaviour, and what pins it is
    /// the sequence the production loop actually sleeps for rather than a
    /// second statement of the formula. [`rename_over`] takes its waits from
    /// here and ends when they run out, so a doubling that stopped doubling, or
    /// an attempt count that moved, changes this list.
    ///
    /// No elapsed time can tell four attempts from one: creating, filling and
    /// syncing the temporary costs more on a busy machine than the whole budget
    /// spends waiting. Widening the budget, which the side-by-side run may well
    /// ask for, is then an edit here as well as above, which is the point.
    #[test]
    fn the_production_budget_waits_two_then_four_then_eight_milliseconds() {
        let budget = RetryBudget::PRODUCTION;

        assert_eq!(budget.attempts, 4, "the production attempt count moved");
        assert_eq!(
            budget.waits().collect::<Vec<_>>(),
            [
                Duration::from_millis(2),
                Duration::from_millis(4),
                Duration::from_millis(8),
            ],
            "the production wait sequence moved"
        );
        assert_eq!(
            budget.waits().sum::<Duration>(),
            Duration::from_millis(14),
            "the production budget no longer waits fourteen milliseconds in total"
        );
    }

    /// A budget that cannot retry yields no waits, so the loop tries once and
    /// reports.
    #[test]
    fn a_budget_with_one_attempt_waits_for_nothing() {
        for attempts in [0, 1] {
            let budget = RetryBudget {
                attempts,
                first_backoff: Duration::from_millis(2),
            };

            assert_eq!(
                budget.waits().count(),
                0,
                "a budget of {attempts} attempts offered a wait"
            );
        }
    }

    /// The temporary has to be a sibling of the target by default. A rename is
    /// only atomic, and only a rename rather than a copy, within one
    /// filesystem, and on the target platform `%APPDATA%` and the system
    /// temporary directory are on different volumes whenever `TEMP` is
    /// redirected to another drive.
    #[test]
    fn the_temporary_sits_in_the_targets_own_directory() {
        let target = Path::new("jdocs").join("jdocs.json");

        let temporary = temporary_path(&target, TemporaryIn::TargetDirectory)
            .expect("naming a temporary failed");

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
        assert!(
            is_temporary_name(&name),
            "the sweep would not recognise {name}"
        );
        assert_ne!(
            temporary,
            temporary_path(&target, TemporaryIn::TargetDirectory)
                .expect("naming a second temporary failed"),
            "two writers of one file would collide on the temporary's name"
        );
    }

    /// A session certificate's temporary goes one directory up, into the pod
    /// root. `ReadSessionCerts` lists `session-certs/` and reads every entry
    /// but `placeholder` as an ESN (`vars.go:371-380`), so a temporary in there
    /// is a bogus robot at best and, through the unchecked `pem.Decode` at
    /// `vars.go:387-388`, a panic at worst.
    #[test]
    fn a_session_certificate_puts_its_temporary_in_the_pod_root() {
        let target = Path::new("wire-pod").join("session-certs").join("00303f28");

        let temporary = temporary_path(&target, TemporaryIn::ParentDirectory)
            .expect("naming a temporary failed");

        assert_eq!(
            temporary.parent(),
            Some(Path::new("wire-pod")),
            "the temporary stayed inside session-certs/"
        );
        let name = temporary
            .file_name()
            .expect("the temporary has no file name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("00303f28."),
            "the temporary {name} is not named after the target"
        );
        assert!(
            is_temporary_name(&name),
            "the sweep would not recognise {name}"
        );
    }

    /// A target with no directory in it resolves against the working directory,
    /// which is where the source layout's own `jdocs/jdocs.json` lives. Asking
    /// for the directory above that one is a caller mistake, and it is reported
    /// rather than guessed at.
    #[test]
    fn a_target_with_no_directory_stays_in_the_working_directory() {
        let temporary = temporary_path(Path::new("jdocs.json"), TemporaryIn::TargetDirectory)
            .expect("naming a temporary failed");

        assert_eq!(
            temporary.parent(),
            Some(Path::new("")),
            "the temporary left the working directory"
        );

        let error = temporary_path(Path::new("jdocs.json"), TemporaryIn::ParentDirectory)
            .expect_err("there is no directory above the working directory to name");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// What the sweep will and will not remove. The pattern is the namer's, and
    /// an operator's own `.tmp` file is not it.
    #[test]
    fn only_this_modules_own_temporary_names_are_swept() {
        for name in [
            "jdocs.json.0123456789abcdef.tmp",
            "botSdkInfo.json.FFFFFFFFFFFFFFFF.tmp",
            "00303f28.00000000000000ff.tmp",
        ] {
            assert!(is_temporary_name(name), "{name} is one of ours");
        }
        for name in [
            "jdocs.json",
            "notes.tmp",
            ".tmp",
            "jdocs.json.0123456789abcde.tmp",
            "jdocs.json.0123456789abcdef0.tmp",
            "jdocs.json.0123456789abcdeg.tmp",
            "jdocs.json.0123456789abcdef.tmp.bak",
            ".0123456789abcdef.tmp",
        ] {
            assert!(!is_temporary_name(name), "{name} is not one of ours");
        }
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
