// Command store-probe records what Go's four transient token stores really do
// when the token server and the jdocs server walk them, so the Rust port is
// checked against the Go toolchain's own answer rather than against a reading
// of the source.
//
// # What is reproduced
//
// Everything below is copied from the Go checkout's chipper/ directory with
// nothing changed that decides a store's contents:
//
//   - pkg/servers/token/token.go:36-45, the four package level slices.
//     TokenHashStore is {target, guid, guidhash}, SecondaryTokenStore is
//     {esn, target, guid, guidhash}, and the session store is split across
//     SessionWriteStoreNames of {target, name} and the parallel
//     SessionWriteStoreCerts.
//   - token.go:130-146, the three removal helpers. Each builds its log line by
//     indexing the slice and then shortens it with
//     append(s[:index], s[index+1:]...), which preserves the order of every
//     surviving element. logger.Debug becomes an append to a slice this program
//     drains per case, so the lines and their order are recorded rather than
//     printed into the middle of the output.
//   - jdocs/server.go:92-109, ReadDocs's walk over the primary store. It is the
//     only one of the three walks with no break, and it removes the element it
//     is standing on, which is what produces the skip and the overrun below.
//   - jdocs/server.go:111-130, ReadDocs's session lookup, which folds case on
//     the stored address split at the first colon and breaks at its first
//     match. The two os.WriteFile calls and the two ini writers between the
//     match and the removal are dropped because none of them touches a store.
//   - jdocs/server.go:81-86, the DeleteData presence check over the same store,
//     which compares with == where the lookup twelve lines later folds case.
//   - token.go:207-217, CreateJWT's scan of the secondary store, which compares
//     serials with == and breaks at its first match.
//
// # The three quirks the recording exists for
//
// A Go range evaluates its expression once, so the primary walk keeps the slice
// header the package variable had when the loop started while the removal
// helper hands the package variable a shorter slice over the same backing
// array. Removing the current element therefore shifts the next one into the
// current index and the loop never visits it, and once the length has dropped
// below the loop's a later match calls the helper with an index the package
// variable no longer has, which the helper indexes to build its log line. The
// primary_skip case records the skip and primary_two_duplicates, together with
// primary_zero_and_two and primary_all_three, record the panic. Every panicking
// case is wrapped in a recover so the store can be read back at the instant
// Go's process would have died.
//
// The session lookup breaks at its first match, so a second entry for the same
// host survives, which session_break records. The secondary store's only
// producer appends an entry and removes it again four statements later
// (jdocs/server.go:136 and :149), so nothing can ever find what it appended;
// secondary_dead_path records that round trip.
//
// # Nothing here is a live value
//
// Every serial, address, GUID, hash and certificate below is a fixed
// placeholder written into this file. The addresses are documentation range
// literals, the GUIDs are g-a and friends, and the certificate bodies are the
// ASCII text cert-a and friends. No path, address or identifier is read from
// this machine or from the running server, and the program opens no file and
// binds no port.
//
// # Output format
//
// Identical to the go-probe and ini-probe recordings beside this one. One case
// per line, three tab separated fields:
//
//	<section>\t<input>\t<output>
//
// Field 2 is space separated key=value pairs whose first pair is always
// kind=<what the line records>; no value contains a space or a tab. Field 3 is
// ALWAYS a Go %q quoted string literal. Lines beginning with '#' are comments
// and there are no blank lines. The pair (field 1, field 2) is unique across
// the file.
//
// A store is encoded as its entries joined with ';', each entry's slots joined
// with '/', in the slot order the Go declaration gives: target/guid/guidhash
// for the primary store, esn/target/guid/guidhash for the secondary, and
// target/name/cert for the session store's two slices read together. An empty
// store encodes as the empty string, and no placeholder in this file contains
// a '/' or a ';'. Several log lines in one case are joined with '\n'; a case
// that logged nothing records the empty string.
//
// The sections are:
//
//	split          strings.Split(addr, ":")[0], the split every reader of a
//	               stored address performs.
//	remove         the three removal helpers called by index, including the
//	               three out of range calls that take Go's process down.
//	primary_walk   ReadDocs's walk over the primary store, skip and overrun.
//	session        the session lookup, its break, and the presence check that
//	               disagrees with it about case.
//	secondary      CreateJWT's scan of the secondary store.
//
// # Regenerating
//
//	cargo run -p xtask -- go-probe            rewrites expected.txt
//	cargo run -p xtask -- go-probe --check    verifies it without rewriting
package main

import (
	"fmt"
	"strings"
)

// --------------------------------------------------------------- the stores

// token.go:36-37, "array of {target, guid, guidhash}".
var TokenHashStore [][3]string

// token.go:40-41, "array of {esn, target, guid, guidhash}".
var SecondaryTokenStore [][4]string

// token.go:44-45, "{target, name}" and the parallel certificate slice.
var (
	SessionWriteStoreNames [][2]string
	SessionWriteStoreCerts [][]byte
)

// One captured logger.Debug call. The bot column is the second argument Go
// passes and the message is the third.
type logLine struct{ bot, msg string }

// Every line the helpers wrote since the last drain, in order.
var logs []logLine

func logDebug(bot, msg string) { logs = append(logs, logLine{bot: bot, msg: msg}) }

// The messages logged since the last call, joined with a newline, and the
// slice reset for the next case.
func drainMsgs() string {
	var out []string
	for _, line := range logs {
		out = append(out, line.msg)
	}
	logs = nil
	return strings.Join(out, "\n")
}

// The bot column of the last line logged since the last drain, or the empty
// string when nothing was logged. Every case below logs at most one line, so
// this reads the column Go passed rather than assuming it.
func lastBot() string {
	if len(logs) == 0 {
		return ""
	}
	return logs[len(logs)-1].bot
}

// ------------------------------------------------- the three removal helpers

// token.go:130-133.
func RemoveFromSecondStore(index int) {
	logDebug(SecondaryTokenStore[index][0], "Removing from temporary token-hash store")
	SecondaryTokenStore = append(SecondaryTokenStore[:index], SecondaryTokenStore[index+1:]...)
}

// token.go:135-138.
func RemoveFromPrimaryStore(index int) {
	logDebug("", "Removing "+TokenHashStore[index][0]+" from temporary token-hash store")
	TokenHashStore = append(TokenHashStore[:index], TokenHashStore[index+1:]...)
}

// token.go:140-146.
func RemoveFromSessionStore(index int) {
	logDebug("", "Removing "+SessionWriteStoreNames[index][0]+" from cert-write store")
	SessionWriteStoreNames = append(SessionWriteStoreNames[:index], SessionWriteStoreNames[index+1:]...)
	SessionWriteStoreCerts = append(SessionWriteStoreCerts[:index], SessionWriteStoreCerts[index+1:]...)
}

// ------------------------------------------------------------- the three walks

// What the last primaryWalk left behind. These are package level rather than
// return values because the walk can panic, and the case still has to read
// what Go's loop had decided before its process would have died.
var (
	walkMatched bool
	walkGUID    string
	walkBody    []string
)

// jdocs/server.go:92-109. The two calls inside the match, WriteTokenHash and
// SetBotGUID, are dropped: neither touches a store, and the four log lines
// around them belong to the jdocs component rather than to the removal helper.
// Everything that decides which element the loop reads next is unchanged.
func primaryWalk(ipAddr string) {
	walkMatched = false
	walkGUID = ""
	walkBody = nil
	for num, pair := range TokenHashStore {
		if strings.EqualFold(pair[0], ipAddr) {
			// The loop body Go runs before the removal, in Go's order.
			walkBody = append(walkBody, pair[1])
			walkGUID = pair[1]
			walkMatched = true
			RemoveFromPrimaryStore(num)
		}
	}
}

// What the last sessionLookup found: Go's num, pair[1] and the certificate it
// wrote, or -1 and two empty strings.
var (
	sessionIndex int
	sessionName  string
	sessionCert  string
)

// jdocs/server.go:111-130.
func sessionLookup(ipAddr string) {
	sessionIndex = -1
	sessionName = ""
	sessionCert = ""
	for num, pair := range SessionWriteStoreNames {
		if strings.EqualFold(ipAddr, strings.Split(pair[0], ":")[0]) {
			sessionIndex = num
			sessionName = pair[1]
			sessionCert = string(SessionWriteStoreCerts[num])
			RemoveFromSessionStore(num)
			break
		}
	}
}

// jdocs/server.go:81-86, the DeleteData presence check. Plain ==, where the
// lookup twelve lines later folds case over the same store.
func sessionPresence(ipAddr string) bool {
	for _, pair := range SessionWriteStoreNames {
		if ipAddr == strings.Split(pair[0], ":")[0] {
			return true
		}
	}
	return false
}

// What the last secondaryWalk found.
var (
	secIndex int
	secGUID  string
	secHash  string
)

// token.go:207-217.
func secondaryWalk(esn string) {
	secIndex = -1
	secGUID = ""
	secHash = ""
	for num, robot := range SecondaryTokenStore {
		if robot[0] == esn {
			secIndex = num
			secGUID = robot[2]
			secHash = robot[3]
			RemoveFromSecondStore(num)
			break
		}
	}
}

// ------------------------------------------------------------------ encoding

func encPrimary(store [][3]string) string {
	var out []string
	for _, e := range store {
		out = append(out, e[0]+"/"+e[1]+"/"+e[2])
	}
	return strings.Join(out, ";")
}

func encSecondary(store [][4]string) string {
	var out []string
	for _, e := range store {
		out = append(out, e[0]+"/"+e[1]+"/"+e[2]+"/"+e[3])
	}
	return strings.Join(out, ";")
}

func encSession(names [][2]string, certs [][]byte) string {
	var out []string
	for i, e := range names {
		out = append(out, e[0]+"/"+e[1]+"/"+string(certs[i]))
	}
	return strings.Join(out, ";")
}

func emit(section, input, output string) {
	fmt.Printf("%s\t%s\t%q\n", section, input, output)
}

// Runs body and answers the recovered runtime error as text, or the empty
// string when it returned normally.
func guard(body func()) string {
	msg := ""
	func() {
		defer func() {
			if r := recover(); r != nil {
				msg = fmt.Sprint(r)
			}
		}()
		body()
	}()
	return msg
}

func main() {
	fmt.Println("# Recorded stdout of docs/phases/P1-robot-connect-auth/store-probe/main.go.")
	fmt.Println("# One case per line: <section>TAB<input>TAB<output>. Field 2 is space separated")
	fmt.Println("# key=value pairs beginning with kind=. Field 3 is ALWAYS a Go %q quoted string")
	fmt.Println("# literal. A store encodes as entries joined with ';', slots joined with '/', in")
	fmt.Println("# the slot order the Go declaration gives. main.go's doc comment names the Go")
	fmt.Println("# source every case reproduces and explains the three quirks.")
	fmt.Println("# Regenerate with: cargo run -p xtask -- go-probe")

	splitSection()
	removeSection()
	primaryWalkSection()
	sessionSection()
	secondarySection()
}

// ----------------------------------------------------------------- split

// strings.Split(addr, ":")[0], which jdocs/server.go:70, :82 and :112 all
// perform and which token.go:202 performs inside a TrimSpace. Go's Split on a
// separator that is absent answers a one element slice, so the [0] never
// panics. The three bracketed addresses are what net.Addr.String() writes for
// an IPv6 peer, and the split cuts inside the address rather than at the port.
func splitSection() {
	const sec = "split"
	for _, addr := range []string{
		"10.0.0.1:50000",
		"10.0.0.1",
		"",
		":50000",
		"[::1]:50000",
		"[::2]:50000",
		"[fe80::1]:443",
	} {
		emit(sec, "kind=split addr="+addr, strings.Split(addr, ":")[0])
	}
}

// ---------------------------------------------------------------- remove

// The three removal helpers called by index. Every case records the store it
// started from, the store it left, the log line's two columns and the panic
// text, so the order of the survivors is Go's own and not an assumption about
// what append(s[:i], s[i+1:]...) does.
func removeSection() {
	const sec = "remove"

	primaryCase := func(name string, store [][3]string, index int) {
		TokenHashStore = store
		logs = nil
		before := encPrimary(TokenHashStore)
		panicked := guard(func() { RemoveFromPrimaryStore(index) })
		bot := lastBot()
		what := fmt.Sprintf("case=%s list=primary index=%d", name, index)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=after "+what, encPrimary(TokenHashStore))
		emit(sec, "kind=log_msg "+what, drainMsgs())
		emit(sec, "kind=log_bot "+what, bot)
		emit(sec, "kind=panic "+what, panicked)
	}

	secondaryCase := func(name string, store [][4]string, index int) {
		SecondaryTokenStore = store
		logs = nil
		before := encSecondary(SecondaryTokenStore)
		panicked := guard(func() { RemoveFromSecondStore(index) })
		bot := lastBot()
		what := fmt.Sprintf("case=%s list=secondary index=%d", name, index)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=after "+what, encSecondary(SecondaryTokenStore))
		emit(sec, "kind=log_msg "+what, drainMsgs())
		emit(sec, "kind=log_bot "+what, bot)
		emit(sec, "kind=panic "+what, panicked)
	}

	sessionCase := func(name string, names [][2]string, certs [][]byte, index int) {
		SessionWriteStoreNames = names
		SessionWriteStoreCerts = certs
		logs = nil
		before := encSession(SessionWriteStoreNames, SessionWriteStoreCerts)
		panicked := guard(func() { RemoveFromSessionStore(index) })
		bot := lastBot()
		what := fmt.Sprintf("case=%s list=session index=%d", name, index)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=after "+what, encSession(SessionWriteStoreNames, SessionWriteStoreCerts))
		emit(sec, "kind=log_msg "+what, drainMsgs())
		emit(sec, "kind=log_bot "+what, bot)
		emit(sec, "kind=panic "+what, panicked)
	}

	three := func() [][3]string {
		return [][3]string{
			{"10.0.0.1", "g-a", "h-a"},
			{"10.0.0.8", "g-b", "h-b"},
			{"10.0.0.9", "g-c", "h-c"},
		}
	}
	threeSecondary := func() [][4]string {
		return [][4]string{
			{"00aaaa01", "10.0.0.7", "g-x", "h-x"},
			{"00aaaa02", "10.0.0.1", "g-y", "h-y"},
			{"00aaaa03", "10.0.0.2", "g-z", "h-z"},
		}
	}
	threeSession := func() ([][2]string, [][]byte) {
		return [][2]string{
				{"10.0.0.1:50000", "Vector-AAA"},
				{"10.0.0.1:50001", "Vector-BBB"},
				{"10.0.0.9:50002", "Vector-CCC"},
			}, [][]byte{
				[]byte("cert-a"),
				[]byte("cert-b"),
				[]byte("cert-c"),
			}
	}

	// Index 0 of three, which is the only index that separates an order
	// preserving removal from one that swaps the last element into the hole.
	primaryCase("primary_remove_first", three(), 0)
	primaryCase("primary_remove_middle", three(), 1)
	primaryCase("primary_remove_out_of_range", [][3]string{{"10.0.0.1", "g-a", "h-a"}}, 1)

	secondaryCase("secondary_remove_first", threeSecondary(), 0)
	// jdocs/server.go:149, the dead path's own removal: the element appended at
	// :136 is the last one, so this is the call the running server makes.
	secondaryCase("secondary_dead_path", [][4]string{
		{"00aaaa01", "10.0.0.7", "g-x", "h-x"},
		{"00aaaa02", "10.0.0.1", "g-y", "h-y"},
	}, 1)
	secondaryCase("secondary_remove_out_of_range", [][4]string{
		{"00aaaa01", "10.0.0.7", "g-x", "h-x"},
	}, 5)

	names, certs := threeSession()
	sessionCase("session_remove_first", names, certs, 0)
	sessionCase("session_remove_out_of_range", [][2]string{{"10.0.0.1:50000", "Vector-AAA"}},
		[][]byte{[]byte("cert-a")}, 1)
}

// ---------------------------------------------------------- primary_walk

// ReadDocs's walk over the primary store. matches is the guid of every entry
// the loop body ran on, in visit order, which is neither the set of entries
// removed nor a deduplicated list: the skip can present one entry to the body
// twice and primary_zero_and_two records exactly that.
func primaryWalkSection() {
	const sec = "primary_walk"

	run := func(name string, store [][3]string, peer string) {
		TokenHashStore = store
		logs = nil
		before := encPrimary(TokenHashStore)
		panicked := guard(func() { primaryWalk(peer) })
		what := fmt.Sprintf("case=%s peer=%s", name, peer)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=matches "+what, strings.Join(walkBody, ";"))
		emit(sec, "kind=after "+what, encPrimary(TokenHashStore))
		emit(sec, "kind=matched "+what, fmt.Sprint(walkMatched))
		emit(sec, "kind=bot_guid "+what, walkGUID)
		emit(sec, "kind=log_msgs "+what, drainMsgs())
		emit(sec, "kind=panic "+what, panicked)
	}

	// Entries 0 and 1 both match: removing entry 0 shifts entry 1 into index 0,
	// which the loop has already passed, so the match at index 1 survives.
	run("primary_skip", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.1", "g-b", "h-b"},
		{"10.0.0.9", "g-c", "h-c"},
	}, "10.0.0.1")

	// The same shift, over an entry that did not match, so the outcome is the
	// obvious one and the removal's log line still names entry 0.
	run("primary_first_only", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.8", "g-b", "h-b"},
		{"10.0.0.9", "g-c", "h-c"},
	}, "10.0.0.1")

	// Nothing has shifted by the time the loop reaches the match.
	run("primary_second_only", [][3]string{
		{"10.0.0.8", "g-a", "h-a"},
		{"10.0.0.1", "g-b", "h-b"},
	}, "10.0.0.1")

	// jdocs/server.go:95 is strings.EqualFold.
	run("primary_equalfold", [][3]string{{"LOCALHOST", "g-a", "h-a"}}, "localhost")

	run("primary_no_match", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.8", "g-b", "h-b"},
		{"10.0.0.9", "g-c", "h-c"},
	}, "10.0.0.2")

	run("primary_empty", nil, "10.0.0.1")

	// Two matching entries, which is one robot asking for a token twice before
	// its ReadDocs arrived, and the smallest store that reaches the panic.
	run("primary_two_duplicates", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.1", "g-b", "h-b"},
	}, "10.0.0.1")

	// Entries 0 and 2 match: entry 2 is shifted into index 1, read there, and
	// then read again from the stale tail at index 2.
	run("primary_zero_and_two", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.8", "g-b", "h-b"},
		{"10.0.0.1", "g-c", "h-c"},
	}, "10.0.0.1")

	run("primary_all_three", [][3]string{
		{"10.0.0.1", "g-a", "h-a"},
		{"10.0.0.1", "g-b", "h-b"},
		{"10.0.0.1", "g-c", "h-c"},
	}, "10.0.0.1")
}

// --------------------------------------------------------------- session

func sessionSection() {
	const sec = "session"

	run := func(name string, names [][2]string, certs [][]byte, peer string) {
		SessionWriteStoreNames = names
		SessionWriteStoreCerts = certs
		logs = nil
		before := encSession(SessionWriteStoreNames, SessionWriteStoreCerts)
		sessionLookup(peer)
		what := fmt.Sprintf("case=%s peer=%s", name, peer)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=found_index "+what, fmt.Sprint(sessionIndex))
		emit(sec, "kind=found_name "+what, sessionName)
		emit(sec, "kind=found_cert "+what, sessionCert)
		emit(sec, "kind=after "+what, encSession(SessionWriteStoreNames, SessionWriteStoreCerts))
		emit(sec, "kind=log_msg "+what, drainMsgs())
	}

	// Two entries for the same host: jdocs/server.go:128 breaks, so the second
	// survives.
	run("session_break", [][2]string{
		{"10.0.0.1:50000", "Vector-AAA"},
		{"10.0.0.1:50001", "Vector-BBB"},
		{"10.0.0.9:50002", "Vector-CCC"},
	}, [][]byte{[]byte("cert-a"), []byte("cert-b"), []byte("cert-c")}, "10.0.0.1")

	run("session_equalfold", [][2]string{{"LOCALHOST:50000", "Vector-AAA"}},
		[][]byte{[]byte("cert-a")}, "localhost")

	run("session_miss", [][2]string{{"10.0.0.9:50002", "Vector-CCC"}},
		[][]byte{[]byte("cert-c")}, "10.0.0.1")

	// An IPv6 peer, whose stored address splits to "[" rather than to a host,
	// so the key the lookup compares is not one either.
	run("session_ipv6", [][2]string{{"[::1]:50000", "Vector-AAA"}},
		[][]byte{[]byte("cert-a")}, "[")

	// jdocs/server.go:82 against jdocs/server.go:112 over one store: the
	// presence check compares with == and the lookup folds case, so a host
	// spelled in another case is found by one and not the other.
	SessionWriteStoreNames = [][2]string{{"LOCALHOST:50000", "Vector-AAA"}}
	SessionWriteStoreCerts = [][]byte{[]byte("cert-a")}
	for _, peer := range []string{"localhost", "LOCALHOST"} {
		what := fmt.Sprintf("case=session_presence peer=%s", peer)
		emit(sec, "kind=store "+what, encSession(SessionWriteStoreNames, SessionWriteStoreCerts))
		emit(sec, "kind=presence "+what, fmt.Sprint(sessionPresence(peer)))
	}
}

// ------------------------------------------------------------- secondary

func secondarySection() {
	const sec = "secondary"

	run := func(name string, store [][4]string, esn string) {
		SecondaryTokenStore = store
		logs = nil
		before := encSecondary(SecondaryTokenStore)
		secondaryWalk(esn)
		bot := lastBot()
		what := fmt.Sprintf("case=%s esn=%s", name, esn)
		emit(sec, "kind=store "+what, before)
		emit(sec, "kind=found_index "+what, fmt.Sprint(secIndex))
		emit(sec, "kind=found_guid "+what, secGUID)
		emit(sec, "kind=found_hash "+what, secHash)
		emit(sec, "kind=after "+what, encSecondary(SecondaryTokenStore))
		emit(sec, "kind=log_msg "+what, drainMsgs())
		emit(sec, "kind=log_bot "+what, bot)
	}

	// Four entries with the match at index 1, so the survivors separate an
	// order preserving removal from one that swaps the last element into the
	// hole, and a duplicate serial after it, so the break at token.go:214 is
	// visible in what survives.
	run("secondary_walk", [][4]string{
		{"00aaaa01", "10.0.0.7", "g-x", "h-x"},
		{"00aaaa02", "10.0.0.1", "g-y", "h-y"},
		{"00aaaa02", "10.0.0.2", "g-z", "h-z"},
		{"00aaaa03", "10.0.0.3", "g-w", "h-w"},
	}, "00aaaa02")

	// token.go:208 is robot[0] == esn, where every other serial lookup in the
	// server folds case.
	run("secondary_case_sensitive", [][4]string{
		{"00AAAA02", "10.0.0.1", "g-y", "h-y"},
	}, "00aaaa02")
}
