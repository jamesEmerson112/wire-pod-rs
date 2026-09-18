//! The three transient stores the token server and the jdocs server share.
//!
//! Go keeps four package-level slices with no lock between them
//! (`token/token.go:37`, `:41`, `:44-45`): a primary store of
//! `{target, guid, guidhash}`, a secondary store of
//! `{esn, target, guid, guidhash}`, and a session-certificate store split
//! across two parallel slices, `SessionWriteStoreNames` of `{target, name}` and
//! `SessionWriteStoreCerts` of raw PEM. Nothing here touches the filesystem.
//! Every entry lives for one association handshake and the whole thing is lost
//! on restart, which is Go's behaviour and not an omission.
//!
//! The four slices become one [`TokenStores`] behind a [`Mutex`] with three
//! typed lists, and the two session slices become one list of
//! [`SessionEntry`], because Go indexes the cert slice with the *name* slice's
//! index (`jdocs/server.go:121`, `:123`) and the two can only be read together.
//!
//! # What each slot means
//!
//! Go's arrays are positional and the meaning of each slot is only visible
//! where it is written and read. Each type below names every slot and keeps
//! Go's order, so an entry can be read back against Go's literal.
//!
//! | Go | Slot 0 | Slot 1 | Slot 2 | Slot 3 |
//! |---|---|---|---|---|
//! | `TokenHashStore [][3]string` | `target` | `guid` | `guidhash` | |
//! | `SecondaryTokenStore [][4]string` | `esn` | `target` | `guid` | `guidhash` |
//! | `SessionWriteStoreNames [][2]string` | `target` | `name` | | |
//!
//! The two `target` slots are not the same shape. The primary store's is a bare
//! host, because `CreateJWT` splits the peer address at the first colon and
//! trims it before appending (`token/token.go:203`, `:236`). The session
//! store's is the whole `net.Addr.String()`, port and all, because
//! `AssociatePrimaryUser` appends `p.Addr.String()` verbatim
//! (`token/token.go:278`), which is why every reader of it splits at the colon
//! again (`jdocs/server.go:82`, `:112`). The secondary store's is the
//! *untrimmed* host `ReadDocs` computed (`jdocs/server.go:70`, `:136`).
//! [`host_of`] is that split, written once.
//!
//! # The three quirks
//!
//! All three were confirmed by running the Go code itself before being encoded
//! here, in a throwaway program that copies `token.go:37-45`, `:130-146`,
//! `jdocs/server.go:81-149` and `token.go:207-217` verbatim with `logger.Debug`
//! replaced by a print. The traces quoted below are that program's stdout on
//! `go1.24.4 windows/amd64`.
//!
//! **A `range` that removes the current element skips the next one.** Go
//! evaluates a range expression once (Go's specification, "For statements with
//! range clause"), so the loop keeps the slice header the package variable had
//! when the loop started: its own length, over the same backing array. The
//! removal helper shifts every later element down inside that array and hands
//! the package variable a shorter slice, so the loop's next read lands on the
//! element that moved *into* the current index and the entry that was there is
//! never visited. `ReadDocs` walks the primary store this way
//! (`jdocs/server.go:94-109`), and it is the only one of the three walks with
//! no `break`. With entries 0 and 1 both matching and entry 2 not, the Go run
//! prints
//!
//! ```text
//! visit num=0 pair0="10.0.0.1" MATCH
//! visit num=1 pair0="10.0.0.9" miss
//! visit num=2 pair0="10.0.0.9" miss
//! after=[[10.0.0.1 g-b h-b] [10.0.0.9 g-c h-c]]
//! ```
//!
//! so entry 1 survives *because* it matched, entry 2 is visited twice, and the
//! caller's `botGUID` is entry 0's. [`TokenStores::take_primary_matches`]
//! reproduces this by modelling the backing array and the package variable's
//! length separately, which is the only way to get the stale tail the loop
//! reads.
//!
//! **That walk can run off the end of its own store.** Once the length has
//! shrunk below the loop's, a later match calls the removal helper with an
//! index the package variable no longer has, and the helper indexes it to build
//! its log line (`token/token.go:136`) and panics before removing anything.
//! Two matching entries are enough, which is one robot asking for a token twice
//! before its `ReadDocs` arrives:
//!
//! ```text
//! visit num=0 pair0="10.0.0.1" MATCH
//! visit num=1 pair0="10.0.0.1" MATCH
//! PANIC runtime error: index out of range [1] with length 1
//! store after=[[10.0.0.1 g-b h-b]]
//! ```
//!
//! The port stops the walk there instead of panicking and reports
//! [`PrimaryWalk::overran`], which leaves the store holding exactly what Go's
//! store holds at the instant its process dies. See "Panics" below.
//!
//! **The session walk stops at its first match, and the secondary path is
//! dead.** `ReadDocs` breaks out of the session loop after writing one
//! certificate (`jdocs/server.go:128`), so a second entry for the same host is
//! left behind; and it appends to the secondary store and then removes the
//! element it just appended (`jdocs/server.go:136`, `:149`), so the store ends
//! the request as it began and nothing ever reads that entry. Both are kept:
//! the append is observable through this type, and the dead path is what
//! [`TokenStores::take_secondary_match`] will keep finding nothing in.
//!
//! # Comparisons
//!
//! Go compares three different ways in four places and the port keeps each one.
//! The primary walk folds case on the whole stored target
//! (`strings.EqualFold`, `jdocs/server.go:95`). The session walk folds case on
//! the stored address split at the colon (`jdocs/server.go:112`). The
//! `DeleteData` presence check a few lines earlier compares the same two values
//! with `==` (`jdocs/server.go:82`), so a peer whose host is spelled in another
//! case is found by one and not the other. The secondary walk compares serials
//! with `==` (`token/token.go:208`), where every other serial lookup in the
//! server uses `EqualFold`. The Go run pins the disagreement:
//! `presence(localhost)=false presence(LOCALHOST)=true` against a stored
//! `LOCALHOST:50000`.
//!
//! `EqualFold` is `eq_ignore_ascii_case` here, as it is everywhere else in this
//! crate (`store/bot_info.rs:86`, deviation 12). Go folds the full Unicode
//! simple case mapping, so a host spelled with U+212A KELVIN SIGN would match
//! `k` there and not here. No peer address or serial can reach this code
//! carrying one.
//!
//! # Panics
//!
//! Nothing here panics. Go's three removal helpers index their slice to build a
//! log line (`token/token.go:131`, `:136`, `:143`) and a caller that passes an
//! index past the end takes the process down; the Go run confirms all three
//! (`index out of range [1] with length 1`, `[5] with length 1`,
//! `[1] with length 1`). Phase 1 already turns Go panics into log lines
//! wherever it finds them, which is reserved deviation 31, and this follows
//! that: the three removals log a line and answer `false`, and
//! [`TokenStores::take_primary_matches`] stops where Go's process would have
//! died. Neither changes what the store holds, because Go panics before it
//! removes anything, so the surviving entries are the same either way; what
//! differs is that the server is still running afterwards. That difference is a
//! candidate numbered deviation for C23.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::logger::COMP_TOKEN;

/// Go's name for the primary store (`token.go:37`), used only to name it in
/// [`report_overrun`]'s line.
const PRIMARY_STORE: &str = "TokenHashStore";

/// Go's `SecondaryTokenStore` (`token.go:41`).
const SECONDARY_STORE: &str = "SecondaryTokenStore";

/// Go's `SessionWriteStoreNames` (`token.go:44`), which is the slice its
/// removal helper indexes first.
const SESSION_STORE: &str = "SessionWriteStoreNames";

// ---------------------------------------------------------------------------
// The entries, slot for slot
// ---------------------------------------------------------------------------

/// One element of Go's `TokenHashStore [][3]string` (`token/token.go:36-37`),
/// whose own comment reads `{"target", "guid", "guidhash"}`.
///
/// `CreateJWT` appends one of these for a robot it could not name, so that the
/// `ReadDocs` that follows can claim it by peer address (`token/token.go:236`).
///
/// `Debug` prints neither secret. The two of them are a robot's bearer token
/// and the digest the association rests on, and a `{:?}` anywhere in a handler
/// would put both into the log ring the web UI serves.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PrimaryEntry {
    /// Slot 0. The peer's host with no port, already split at the first colon
    /// and trimmed by the caller (`token/token.go:203`).
    pub target: String,
    /// Slot 1. The GUID handed to the robot.
    pub guid: String,
    /// Slot 2. The hash written into `vic.AppTokens`.
    pub guid_hash: String,
}

impl fmt::Debug for PrimaryEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrimaryEntry")
            .field("target", &self.target)
            .field("guid", &Redacted(self.guid.len()))
            .field("guid_hash", &Redacted(self.guid_hash.len()))
            .finish()
    }
}

/// One element of Go's `SecondaryTokenStore [][4]string`
/// (`token/token.go:40-41`), whose own comment reads
/// `{"esn", "target", "guid", "guidhash"}`.
///
/// `ReadDocs` appends one for a robot wire-pod has never seen and removes it
/// again four statements later (`jdocs/server.go:136`, `:149`), so in the
/// running server this list is empty every time anything reads it. The store is
/// still here because the append and the removal are both observable and
/// because `CreateJWT` still scans it (`token/token.go:207`).
///
/// `Debug` redacts the two secrets, as [`PrimaryEntry`] does.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecondaryEntry {
    /// Slot 0. The robot's serial, compared with `==` rather than `EqualFold`
    /// (`token/token.go:208`).
    pub esn: String,
    /// Slot 1. The peer's host with no port, as `ReadDocs` computed it, which
    /// is untrimmed where the primary store's is trimmed
    /// (`jdocs/server.go:70`).
    pub target: String,
    /// Slot 2. The GUID.
    pub guid: String,
    /// Slot 3. The hash.
    pub guid_hash: String,
}

impl fmt::Debug for SecondaryEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecondaryEntry")
            .field("esn", &self.esn)
            .field("target", &self.target)
            .field("guid", &Redacted(self.guid.len()))
            .field("guid_hash", &Redacted(self.guid_hash.len()))
            .finish()
    }
}

/// One element of Go's two parallel session slices, merged
/// (`token/token.go:44-45`).
///
/// `AssociatePrimaryUser` appends the certificate first and the
/// `{target, name}` pair second, with no lock between them
/// (`token/token.go:276-278`), and `RemoveFromSessionStore` shortens both by
/// the same index (`token/token.go:144-145`). Merging them is a deliberate
/// difference: two concurrent associations can interleave the two appends in Go
/// and leave the lists a different length, which the Go run reproduces (a
/// removal at index 0 with one name and two certificates leaves the orphan
/// certificate behind), and a later reader then indexes the wrong certificate
/// or panics. One list cannot reach that state. It is a candidate numbered
/// deviation for C23.
///
/// `Debug` prints the certificate's length rather than its bytes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SessionEntry {
    /// Slot 0 of `SessionWriteStoreNames`: the whole peer address as
    /// `net.Addr.String()` writes it, port included
    /// (`token/token.go:278`). Every reader splits it with [`host_of`].
    pub peer_addr: String,
    /// Slot 1: the common name of the session certificate's issuer, which is
    /// the robot's `Vector-XXXX` name and becomes part of the exported
    /// certificate's filename (`token/token.go:278`, `jdocs/server.go:114`).
    pub name: String,
    /// The certificate itself, exactly the PEM bytes the request carried
    /// (`token/token.go:276`). A `Vec<u8>` and not a parsed certificate,
    /// because this crate depends on neither `wirepod-proto` nor a TLS stack
    /// and because Go stores the request's bytes rather than the parse.
    pub cert: Vec<u8>,
}

impl fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEntry")
            .field("peer_addr", &self.peer_addr)
            .field("name", &self.name)
            .field("cert", &Redacted(self.cert.len()))
            .finish()
    }
}

/// A placeholder that prints a length instead of a value, so that a `Debug` of
/// any entry above can be logged without putting a secret in the log ring.
struct Redacted(usize);

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// Where a session entry was found and what it holds.
///
/// The index is separate from the entry because `ReadDocs` writes two files and
/// two more records between finding the entry and removing it
/// (`jdocs/server.go:113-126`), and every one of those writes a log line. A
/// lookup that removed as it found would put
/// `Removing <target> from cert-write store` before
/// `Outputting session cert to <path>` rather than between it and
/// `Session certificate successfully output`, so the caller is handed the index
/// and calls [`TokenStores::remove_from_session_store`] at Go's statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMatch {
    /// The index to hand [`TokenStores::remove_from_session_store`].
    pub index: usize,
    /// A copy of the entry, so the caller can write the certificate without
    /// holding the store's lock.
    pub entry: SessionEntry,
}

/// What one [`TokenStores::take_primary_matches`] walk did.
///
/// Go's loop keeps two locals, `matched` and `botGUID` (`jdocs/server.go:92`,
/// `:93`), and assigns `botGUID` on every match, so the value it leaves is the
/// *last* match's. [`Self::matched`] and [`Self::bot_guid`] are those two.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrimaryWalk {
    /// Every entry the loop body ran on, in visit order.
    ///
    /// This is not the same as the entries removed and it is not deduplicated:
    /// the skip quirk can present one entry to the body twice, and the Go run
    /// shows exactly that. Each element is what the loop read, so a caller that
    /// mirrors Go's body runs once per element here.
    pub matches: Vec<PrimaryEntry>,
    /// True when the walk stopped because Go's next removal would have indexed
    /// past the end of the store and panicked.
    ///
    /// The last element of [`Self::matches`] is the entry that match belonged
    /// to. Go runs its loop body on it too, before the removal helper dies.
    pub overran: bool,
}

impl PrimaryWalk {
    /// Go's `matched` (`jdocs/server.go:92`, `:106`), which decides whether
    /// `ReadDocs` falls through to the global-GUID answer.
    pub fn matched(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Go's `botGUID` (`jdocs/server.go:93`, `:101`): the last match's GUID, or
    /// the empty string when nothing matched.
    pub fn bot_guid(&self) -> &str {
        self.matches.last().map_or("", |entry| entry.guid.as_str())
    }
}

/// Go's `strings.Split(addr, ":")[0]` (`jdocs/server.go:70`, `:82`, `:112`,
/// `token/token.go:203`).
///
/// Everything before the first colon, or the whole string when there is none.
/// Go's `Split` on a separator that is absent answers a one-element slice, so
/// its `[0]` never panics and neither does this, and an empty address answers
/// the empty string.
///
/// This degenerates on IPv6. A gRPC peer address is `host:port`, and for an
/// IPv6 peer Go's own `net.Addr.String()` writes `[::1]:50000`, whose first
/// colon is inside the address. The Go run prints `"[::1]:50000" -> "["` and
/// `"[fe80::1]:443" -> "[fe80"`, so the result is never a host and two
/// different peers can share one. That is a Go bug this reproduces rather than
/// fixes, because the primary store's own targets were cut the same way on the
/// way in and the two halves have to agree.
pub fn host_of(peer_addr: &str) -> &str {
    match peer_addr.find(':') {
        Some(at) => &peer_addr[..at],
        None => peer_addr,
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// The three lists behind the one mutex.
#[derive(Debug, Default)]
struct Stores {
    /// Go's `TokenHashStore` (`token.go:37`).
    primary: Vec<PrimaryEntry>,
    /// Go's `SecondaryTokenStore` (`token.go:41`).
    secondary: Vec<SecondaryEntry>,
    /// Go's `SessionWriteStoreNames` and `SessionWriteStoreCerts`, merged
    /// (`token.go:44-45`).
    session: Vec<SessionEntry>,
}

/// The transient token stores, replacing four of Go's unsynchronized globals.
///
/// One of these is built at startup and shared through
/// [`AppState`](crate::state::AppState). Go's four slices are package variables
/// that the token server's three RPCs and the jdocs server's `ReadDocs` mutate
/// from different connections' goroutines with nothing between them; one mutex
/// over all three lists is what makes the walks below atomic, which the skip
/// quirk needs in order to be reproducible at all.
///
/// Every method is synchronous and no guard outlives one, so nothing here can
/// trip the crate's `deny(clippy::await_holding_lock)` and a caller can drive
/// the whole type from a plain `#[test]` with no runtime.
///
/// `Debug` prints the three lengths and no entry, so that a `{:?}` of
/// [`AppState`](crate::state::AppState) cannot put a GUID in the log ring.
#[derive(Default)]
pub struct TokenStores {
    inner: Mutex<Stores>,
}

impl TokenStores {
    /// Three empty lists, which is the state Go's four globals are in at
    /// startup and after any restart.
    pub fn new() -> Self {
        Self::default()
    }

    // -- appends ------------------------------------------------------------

    /// Go's `TokenHashStore = append(...)` (`token/token.go:236`).
    ///
    /// Silent, because Go's log line for this one sits at the call site rather
    /// than in a helper: `CreateJWT` writes `Adding <ip> to TokenHashStore`
    /// immediately before the append (`token/token.go:234`), and it belongs to
    /// whichever commit ports `CreateJWT`.
    pub fn add_primary(&self, entry: PrimaryEntry) {
        self.lock().primary.push(entry);
    }

    /// Go's `SecondaryTokenStore = append(...)` (`jdocs/server.go:136`).
    ///
    /// The only call site removes the entry again at `jdocs/server.go:149`, so
    /// a caller that ports `ReadDocs` follows this with
    /// `remove_from_second_store(secondary_len() - 1)`. Keeping the dead path
    /// is deliberate: it is the shape a future Go change would grow back into,
    /// and its removal log line is a line the running server really writes.
    pub fn add_secondary(&self, entry: SecondaryEntry) {
        self.lock().secondary.push(entry);
    }

    /// Go's two appends in `AssociatePrimaryUser` (`token/token.go:276-278`),
    /// which this makes one.
    ///
    /// Go appends the certificate before the pair; merged, the ordering has
    /// nothing to expose. [`SessionEntry`] says why the merge is a deliberate
    /// difference.
    pub fn add_session(&self, entry: SessionEntry) {
        self.lock().session.push(entry);
    }

    // -- removals by index --------------------------------------------------

    /// Go's `RemoveFromPrimaryStore` (`token/token.go:135-138`).
    ///
    /// The log line names the entry being removed and carries no bot, because
    /// Go interpolates the target into the message and passes `""` for the bot
    /// column.
    ///
    /// Answers whether anything was removed. Go has no answer and panics
    /// instead; the module's "Panics" section says why this does not.
    pub fn remove_from_primary_store(&self, index: usize) -> bool {
        let mut inner = self.lock();
        if index >= inner.primary.len() {
            report_overrun(PRIMARY_STORE, index, inner.primary.len());
            return false;
        }
        tracing::debug!(
            target: COMP_TOKEN,
            "Removing {} from temporary token-hash store", inner.primary[index].target
        );
        inner.primary.remove(index);
        true
    }

    /// Go's `RemoveFromSecondStore` (`token/token.go:130-133`).
    ///
    /// This is the one removal whose line carries a bot: Go passes
    /// `SecondaryTokenStore[index][0]`, which is the serial, as the bot column
    /// and leaves the message with nothing interpolated into it.
    pub fn remove_from_second_store(&self, index: usize) -> bool {
        let mut inner = self.lock();
        if index >= inner.secondary.len() {
            report_overrun(SECONDARY_STORE, index, inner.secondary.len());
            return false;
        }
        tracing::debug!(
            target: COMP_TOKEN,
            bot = %inner.secondary[index].esn,
            "Removing from temporary token-hash store"
        );
        inner.secondary.remove(index);
        true
    }

    /// Go's `RemoveFromSessionStore` (`token/token.go:140-146`), which shortens
    /// both of its slices by the same index.
    ///
    /// The log line names the whole peer address, port included, because that
    /// is what slot 0 holds.
    pub fn remove_from_session_store(&self, index: usize) -> bool {
        let mut inner = self.lock();
        if index >= inner.session.len() {
            report_overrun(SESSION_STORE, index, inner.session.len());
            return false;
        }
        tracing::debug!(
            target: COMP_TOKEN,
            "Removing {} from cert-write store", inner.session[index].peer_addr
        );
        inner.session.remove(index);
        true
    }

    // -- the consumer walks -------------------------------------------------

    /// `ReadDocs`'s walk over the primary store (`jdocs/server.go:94-109`),
    /// skip quirk and all.
    ///
    /// `peer_ip` is the host the caller already split out of the peer address
    /// with [`host_of`], because Go splits it once at `jdocs/server.go:70` and
    /// the loop compares the whole stored target against it. The comparison is
    /// `EqualFold` (`jdocs/server.go:95`).
    ///
    /// The implementation models Go's slice over its backing array rather than
    /// iterating the list, because that is the whole behaviour. `array` is the
    /// backing array at its original length, which is what the loop reads;
    /// `length` is the package variable's own length, which is what the removal
    /// shortens and what bounds the removal's index. A removal copies every
    /// later element down inside `array` and leaves the last slot holding its
    /// old value, which is exactly what `append(s[:i], s[i+1:]...)` does and is
    /// the stale entry the loop goes on to read. Truncating to `length` at the
    /// end is what hands the shortened slice back.
    ///
    /// Go runs the body of the loop, which writes the token hash, sets the bot
    /// GUID and writes two files, between the match and the removal. Those
    /// cannot happen here: they are `async` and this holds a
    /// [`std::sync::Mutex`]. The caller runs them afterwards, once per element
    /// of [`PrimaryWalk::matches`], so the removal's log line comes before
    /// their log lines rather than after. That reordering is a candidate
    /// numbered deviation for C23. The control flow cannot be split the same
    /// way, because which element the loop reads next depends on the removal
    /// having happened.
    pub fn take_primary_matches(&self, peer_ip: &str) -> PrimaryWalk {
        let mut inner = self.lock();

        let mut array = std::mem::take(&mut inner.primary);
        // The length Go's `range` expression captured, which never changes.
        let total = array.len();
        // The package variable's own length, which every removal shortens.
        let mut length = total;
        let mut walk = PrimaryWalk::default();

        for num in 0..total {
            if !array[num].target.eq_ignore_ascii_case(peer_ip) {
                continue;
            }
            // Go's loop body runs here, before `RemoveFromPrimaryStore`.
            walk.matches.push(array[num].clone());

            if num >= length {
                // `token/token.go:136` indexes the shortened slice to build its
                // log line and panics. Go removes nothing and never reaches the
                // next iteration, so neither does this.
                walk.overran = true;
                report_overrun(PRIMARY_STORE, num, length);
                break;
            }

            tracing::debug!(
                target: COMP_TOKEN,
                "Removing {} from temporary token-hash store", array[num].target
            );
            for slot in num..length - 1 {
                let next = array[slot + 1].clone();
                array[slot] = next;
            }
            length -= 1;
        }

        array.truncate(length);
        inner.primary = array;
        walk
    }

    /// `CreateJWT`'s scan of the secondary store (`token/token.go:207-217`).
    ///
    /// The serial is compared with `==` and the loop breaks on the first match,
    /// so no element is ever skipped and a second entry for the same serial is
    /// left where it is. The matched entry is removed, which is Go's
    /// `RemoveFromSecondStore(num)` at `token/token.go:213`, and its log line
    /// is written there.
    ///
    /// Finding *and* removing in one call is safe where the session lookup's
    /// equivalent is not: Go's four statements between the match and the
    /// removal are local assignments (`token/token.go:209-212`) that touch no
    /// store and write no log line, so nothing can be observed between them.
    ///
    /// In the running server this always answers `None`, because the only thing
    /// that appends to this store removes the entry again in the same request.
    pub fn take_secondary_match(&self, esn: &str) -> Option<SecondaryEntry> {
        let mut inner = self.lock();
        let index = inner.secondary.iter().position(|entry| entry.esn == esn)?;
        tracing::debug!(
            target: COMP_TOKEN,
            bot = %inner.secondary[index].esn,
            "Removing from temporary token-hash store"
        );
        Some(inner.secondary.remove(index))
    }

    /// `ReadDocs`'s lookup in the session store (`jdocs/server.go:111-130`),
    /// which stops at the first match.
    ///
    /// `peer_ip` is the caller's already-split host; the *stored* address is
    /// split here, because Go splits it inside the loop
    /// (`jdocs/server.go:112`). The comparison is `EqualFold`.
    ///
    /// Nothing is removed. [`SessionMatch`] says why the removal is the
    /// caller's to make.
    pub fn find_session_match(&self, peer_ip: &str) -> Option<SessionMatch> {
        let inner = self.lock();
        let index = inner
            .session
            .iter()
            .position(|entry| peer_ip.eq_ignore_ascii_case(host_of(&entry.peer_addr)))?;
        Some(SessionMatch {
            index,
            entry: inner.session[index].clone(),
        })
    }

    /// `ReadDocs`'s earlier presence check over the same store
    /// (`jdocs/server.go:81-86`), which decides whether to drop the robot's
    /// documents before reading them.
    ///
    /// This is the comparison that is **not** `EqualFold`: Go writes
    /// `ipAddr == strings.Split(pair[0], ":")[0]` (`jdocs/server.go:82`), so a
    /// stored address whose host differs only in case is found by
    /// [`Self::find_session_match`] and not by this. The Go run pins it.
    pub fn session_holds(&self, peer_ip: &str) -> bool {
        self.lock()
            .session
            .iter()
            .any(|entry| peer_ip == host_of(&entry.peer_addr))
    }

    // -- reads --------------------------------------------------------------

    /// A copy of the primary store, for a caller that has to hold it across an
    /// `.await` and for a test.
    pub fn primary_snapshot(&self) -> Vec<PrimaryEntry> {
        self.lock().primary.clone()
    }

    /// A copy of the secondary store.
    pub fn secondary_snapshot(&self) -> Vec<SecondaryEntry> {
        self.lock().secondary.clone()
    }

    /// A copy of the session store.
    pub fn session_snapshot(&self) -> Vec<SessionEntry> {
        self.lock().session.clone()
    }

    /// How many entries the primary store holds.
    pub fn primary_len(&self) -> usize {
        self.lock().primary.len()
    }

    /// How many entries the secondary store holds. `ReadDocs` needs this to
    /// name the element it has just appended (`jdocs/server.go:149`).
    pub fn secondary_len(&self) -> usize {
        self.lock().secondary.len()
    }

    /// How many entries the session store holds.
    pub fn session_len(&self) -> usize {
        self.lock().session.len()
    }

    /// Go's mutex-free globals have no poisoning to recover from, so a guard
    /// poisoned by a panic in another thread is taken anyway. Every method here
    /// leaves the three lists consistent, so there is no torn state to protect
    /// a later caller from.
    fn lock(&self) -> MutexGuard<'_, Stores> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for TokenStores {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("TokenStores")
            .field("primary", &inner.primary.len())
            .field("secondary", &inner.secondary.len())
            .field("session", &inner.session.len())
            .finish()
    }
}

/// The line written where Go's removal helper would have panicked.
///
/// Go has no line here and no `else` branch: it indexes the slice, the runtime
/// stops the process, and nothing is removed. This says the same thing to the
/// log ring at `WARN`, which is a level the web UI's `/api/get_logs` buffer
/// shows, because a store that has overrun means two entries claimed the same
/// peer and someone should see it.
fn report_overrun(store: &str, index: usize, length: usize) {
    tracing::warn!(
        target: COMP_TOKEN,
        "index {index} is out of range for {store}, which holds {length}; \
         Go panics here, so nothing was removed"
    );
}
