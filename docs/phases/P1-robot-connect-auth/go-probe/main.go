// Command go-probe records the Go behaviors that Phase 1 of the Rust port of
// wire-pod has to reproduce byte for byte. Every input is a fixed constant
// written into this file. Nothing is read from the clock, a map iteration or a
// live server, so two runs on any machine produce identical bytes and the
// recording is a diffable artifact. The one thing the program reads off the
// host is the IANA time zone database, which time.LoadLocation finds through
// the ZONEINFO environment variable, then the platform's zoneinfo directories,
// then the copy time/tzdata embeds; in Go's own source tree that search is
// src/time/zoneinfo.go:663-696 and src/time/zoneinfo_read.go:531-569. See "The
// zone the addmonth_local section uses" below for why that still leaves the
// recording stable.
//
// # Sections, and the Go source each one reproduces
//
// Paths below are relative to the Go checkout's chipper/ directory except
// where they name Go's own source tree.
//
//	hash         pkg/servers/token/hashing.go:16-26 (the sizes and the error
//	             strings), :47-71 (CreateTokenAndHashedToken), :89-114
//	             (CompareHashAndToken), :116-128 (newFromHash) and :130-136
//	             (hash). GUID = base64.StdEncoding of the 16 token bytes; the
//	             stored hash = base64.StdEncoding of sha256(token||salt)
//	             followed by the 16 salt bytes, so 48 raw bytes and 64 base64
//	             characters.
//	rfc3339      pkg/servers/token/token.go:29, which sets TimeFormat to
//	             time.RFC3339Nano, the layout used for the "iat" and "expires"
//	             claims. RFC3339Nano is "2006-01-02T15:04:05.999999999Z07:00":
//	             the nines mean the fraction is printed with trailing zeros
//	             removed, and a fraction of exactly zero drops the decimal
//	             point too.
//	addmonth     pkg/servers/token/token.go:196, time.Time.AddDate(0, 1, 0),
//	             which is how the "expires" claim lands one calendar month out.
//	             AddDate adds to the month field and then lets time.Date
//	             normalise, so 31 January plus one month is 3 March in a common
//	             year and 2 March in a leap year. Every case in this section
//	             runs in a time.FixedZone, which has one offset for all time.
//	addmonth_local
//	             The same AddDate(0, 1, 0) call in a zone that has daylight
//	             saving transitions, which is the situation token.go:195-196 is
//	             really in: both timestamps come from time.Now(), so their
//	             location is time.Local and not a fixed offset. See "The zone
//	             the addmonth_local section uses" below for the two rules only
//	             a real zone can show.
//	f32json      pkg/vars/config.go:38-39, the top_p and temp fields, which are
//	             declared float32 and rendered by encoding/json's floatEncoder.
//	             In Go's own source tree that encoder is
//	             src/encoding/json/encode.go:538-572. The bit width comes from
//	             encode.go:574-577, where float32Encoder is floatEncoder(32) and
//	             float64Encoder is floatEncoder(64), so a float32 field really
//	             is formatted at 32 bits and 0.7 stays "0.7". The 'f' versus 'e'
//	             choice is encode.go:556-560, which for bits == 32 compares
//	             float32(abs) against 1e-6 and 1e21. The cleanup that rewrites a
//	             two digit negative exponent, e-09 to e-9, is encode.go:562-569;
//	             it fires only when the sign byte is '-', so 1e+21 keeps its
//	             zero.
//	claims       pkg/servers/token/token.go:254-265, the seven key JWT claim
//	             payload. See "How the claims section is built" below.
//	legacystamp  pkg/logger/logger.go:102-111 (legacyLine) and :113-122
//	             (fileFormatLine), both of which stamp with the layout
//	             "2006.01.02 15:04:05", plus pkg/logger/logger.go:20-31 for
//	             Level.String.
//
// # The zone the addmonth_local section uses
//
// A time.FixedZone carries one offset for all time, so the addmonth and
// rfc3339 sections above cannot show either of the two rules that decide what
// the Go server actually writes into the "expires" claim:
//
//  1. Format prints the offset in effect at the instant being formatted, not
//     the one the source instant carried. "iat" and "expires" are one calendar
//     month apart, so for roughly a month before each transition they carry
//     different offsets.
//  2. When the target wall time does not exist, because a spring forward
//     transition removed it, time.Date normalises backwards by the size of the
//     gap rather than forwards. 8 February 02:30 plus one month is 8 March
//     01:30, an hour earlier on the wall clock than the 02:30 that was asked
//     for. When the target wall time happens twice, because a fall back
//     transition repeated it, time.Date picks the first of the two.
//
// The zone is America/Los_Angeles, whose rules are the US ones fixed by the
// Energy Policy Act of 2005 and unchanged since 2007: forward at 02:00 local
// on the second Sunday in March, back at 02:00 local on the first Sunday in
// November. Every instant recorded in that section is in 2026 or 2027, so no
// future tzdata release can move one of these cases: a release revises
// historical rules or announces a new one, and the last US change was the 2005
// act taking effect in 2007.
//
// time/tzdata is imported for its side effect so LoadLocation still resolves on
// a host that carries no zoneinfo files, which is every stock Windows machine.
// It is standard library, so it adds nothing to go.mod.
//
// # How the claims section is built
//
// The claims are built with the real library the Go server uses,
// github.com/golang-jwt/jwt v3.2.2+incompatible, the version pinned by the Go
// checkout's chipper/go.mod. The values are jwt.MapClaims and
// jwt.SigningMethodRS512, exactly the two the Go server names at token.go:254,
// and the header map is produced by jwt.NewWithClaims rather than written out
// here.
//
// The payload bytes are json.Marshal of the claims and the header bytes are
// json.Marshal of the header, which is what the library itself does: see
// token.go:65-83 in that module, where SigningString marshals t.Header and then
// t.Claims and base64url encodes each with EncodeSegment. EncodeSegment is
// base64.RawURLEncoding, that is the URL alphabet with padding stripped
// (token.go:97-99 in that module). jwt.MapClaims is a plain
// map[string]interface{} (map_claims.go:11 in that module), so Go's map encoder
// decides the key order: it sorts keys with strings.Compare
// (src/encoding/json/encode.go:745-775), which is byte order over the raw key
// strings. That is why the key order recorded here is authoritative rather than
// an accident of this program.
//
// Nothing in this section is a live value. The requestor id, token id and
// timestamps are fixed placeholders chosen here; the only strings taken from
// the Go server are the two literals it hard codes, recorded as consts.
//
// # Output format
//
// One case per line, three tab separated fields:
//
//	<section>\t<input>\t<output>
//
// Field 1 is the section name from the list above.
//
// Field 2 describes the input as space separated key=value pairs. The first
// pair is always kind=<what the line records>. Keys match [a-z_]+ and no value
// contains a space or a tab, so splitting field 2 on ' ' and then each piece on
// its first '=' recovers the inputs. A value may be empty, as in "comp=".
//
// Field 3 is ALWAYS a Go %q quoted string literal, even when the value is a
// short number with nothing to escape, so one unquoting rule covers every line.
// Go's %q emits a Go string literal: double quoted, with backslash escapes for
// a backslash, a double quote, a newline, a carriage return and a tab, hex
// escapes for other non printable bytes, and printable ASCII left as itself.
// Every byte this program emits is ASCII, so the only escapes that appear in
// practice are the five named ones, all of which Rust spells the same way.
//
// Lines whose first character is '#' are comments. There are no blank lines.
// The pair (field 1, field 2) is unique across the whole file, so it is safe to
// key a map on it. Sections appear in the order listed above and cases appear
// in the order this program writes them; both are stable.
//
// # Regenerating
//
// expected.txt in this directory is the recorded stdout of this program.
// Rewrite it, and the ini-probe recording beside it, with
//
//	cargo run -p xtask -- go-probe
//
// and check the committed recordings without rewriting anything with
//
//	cargo run -p xtask -- go-probe --check
package main

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math"
	"strings"
	"time"
	_ "time/tzdata"

	"github.com/golang-jwt/jwt"
)

// Copied from pkg/servers/token/hashing.go:16-26 so this program depends on no
// wire-pod code: the Go checkout is read only and its module is not imported.
const (
	tokenSize = 16
	saltSize  = 16
	hashSize  = sha256.Size

	errMismatchedTokenAndHash = "hash mismatch"
	errHashTooLong            = "hash too long"
	errHashTooShort           = "hash too short"
	errTokenTooLong           = "token too long"
	errTokenTooShort          = "token too short"
)

// hashed mirrors hashing.go:28-34.
type hashed struct {
	hash []byte
	salt []byte
}

// goHash is hashing.go:130-136 verbatim, renamed only to leave the name hashed
// free for the struct above.
func goHash(token, salt []byte) []byte {
	salted := make([]byte, 0, (tokenSize + saltSize))
	salted = append(salted, token...)
	salted = append(salted, salt...)
	h := sha256.Sum256(salted)
	return h[:]
}

// newFromHash is hashing.go:116-128 verbatim.
func newFromHash(hashedToken []byte) (*hashed, error) {
	if len(hashedToken) > (hashSize + saltSize) {
		return nil, fmt.Errorf(errHashTooLong)
	} else if len(hashedToken) < (hashSize + saltSize) {
		return nil, fmt.Errorf(errHashTooShort)
	}

	h, salt := hashedToken[:hashSize], hashedToken[hashSize:]
	return &hashed{hash: h, salt: salt}, nil
}

// compareHashAndToken is hashing.go:89-114 with the constant time compare
// replaced by a string comparison, which changes the timing and not the answer.
//
// This direction is dead in the Go checkout: CompareHashAndToken's only caller
// is DecodeAndCompare at hashing.go:73-84, and nothing calls that. It is
// recorded because the Rust side uses it for the live association check against
// the hash already on disk, so the answers below are a contract for the port
// rather than a behaviour the Go server exercises.
func compareHashAndToken(hashedToken, token string) error {
	hashedBytes, err := base64.StdEncoding.DecodeString(hashedToken)
	if err != nil {
		return err
	}
	tokenBytes, err := base64.StdEncoding.DecodeString(token)
	if err != nil {
		return err
	}
	h, err := newFromHash(hashedBytes)
	if err != nil {
		return err
	}
	newHash := goHash(tokenBytes, h.salt)
	if string(h.hash) == string(newHash) {
		return nil
	}
	return fmt.Errorf(errMismatchedTokenAndHash)
}

// seen keys every line already emitted by its section and input, so a case
// list that covers the same input twice records it once. A repeat that carries
// a different output is a bug in this program, not a case, so it panics rather
// than emitting an ambiguous pair.
var seen = map[string]string{}

func emit(section, input, output string) {
	key := section + "\t" + input
	if prev, ok := seen[key]; ok {
		if prev != output {
			panic("same input recorded twice with different outputs: " + key)
		}
		return
	}
	seen[key] = output
	fmt.Printf("%s\t%s\t%q\n", section, input, output)
}

func main() {
	fmt.Println("# Recorded stdout of docs/phases/P1-robot-connect-auth/go-probe/main.go.")
	fmt.Println("# One case per line: <section>TAB<input>TAB<output>. Field 2 is space separated")
	fmt.Println("# key=value pairs beginning with kind=. Field 3 is ALWAYS a Go %q quoted string")
	fmt.Println("# literal, so one unquoting rule covers every line. main.go's doc comment names")
	fmt.Println("# the Go source each section reproduces.")
	fmt.Println("# Regenerate with: cargo run -p xtask -- go-probe")
	fmt.Println("# Verify with:     cargo run -p xtask -- go-probe --check")

	hashSection()
	rfc3339Section()
	addMonthSection()
	addMonthLocalSection()
	f32JSONSection()
	claimsSection()
	legacyStampSection()
}

// ------------------------------------------------------------------- hash

type hashCase struct {
	token [tokenSize]byte
	salt  [saltSize]byte
}

func pattern(f func(i int) byte) [16]byte {
	var b [16]byte
	for i := range b {
		b[i] = f(i)
	}
	return b
}

func hashSection() {
	const sec = "hash"

	zeros := pattern(func(int) byte { return 0x00 })
	ones := pattern(func(int) byte { return 0xff })
	asc := pattern(func(i int) byte { return byte(i) })
	desc := pattern(func(i int) byte { return byte(15 - i) })
	mixA := pattern(func(i int) byte { return byte(0xa5 ^ (i * 17)) })
	mixB := pattern(func(i int) byte { return byte(0x5a + i*3) })

	cases := []hashCase{
		{token: zeros, salt: zeros},
		{token: ones, salt: ones},
		{token: asc, salt: desc},
		{token: desc, salt: asc},
		{token: mixA, salt: mixB},
		{token: mixB, salt: mixB}, // token and salt equal
		{token: zeros, salt: ones},
		{token: ones, salt: zeros},
	}

	for _, c := range cases {
		tok := c.token[:]
		salt := c.salt[:]
		emit(sec, fmt.Sprintf("kind=guid token=%x", tok),
			base64.StdEncoding.EncodeToString(tok))

		raw := append(goHash(tok, salt), salt...)
		emit(sec, fmt.Sprintf("kind=hash token=%x salt=%x", tok, salt),
			base64.StdEncoding.EncodeToString(raw))
	}

	// Sizes, so the Rust side never guesses a length.
	emit(sec, "kind=const name=tokenSize", fmt.Sprint(tokenSize))
	emit(sec, "kind=const name=saltSize", fmt.Sprint(saltSize))
	emit(sec, "kind=const name=hashSize", fmt.Sprint(hashSize))
	emit(sec, "kind=const name=hashed_raw_len", fmt.Sprint(hashSize+saltSize))
	emit(sec, "kind=const name=guid_b64_len",
		fmt.Sprint(len(base64.StdEncoding.EncodeToString(make([]byte, tokenSize)))))
	emit(sec, "kind=const name=hashed_b64_len",
		fmt.Sprint(len(base64.StdEncoding.EncodeToString(make([]byte, hashSize+saltSize)))))

	// The error strings, verbatim from hashing.go:21-25. Only the first three
	// are reachable. errTokenTooLong and errTokenTooShort are declared at
	// hashing.go:24-25 and referenced nowhere else in the Go checkout: hash()
	// at :130-136 appends a token of any length, and CompareHashAndToken at
	// :89-114 never looks at len(tokenBytes). Go therefore never produces
	// either string. They are recorded for completeness, and the Rust side must
	// not add a token length check; the short and long kind=compare cases below
	// pin what Go answers instead.
	emit(sec, "kind=const name=errMismatchedTokenAndHash", errMismatchedTokenAndHash)
	emit(sec, "kind=const name=errHashTooLong", errHashTooLong)
	emit(sec, "kind=const name=errHashTooShort", errHashTooShort)
	emit(sec, "kind=const name=errTokenTooLong", errTokenTooLong)
	emit(sec, "kind=const name=errTokenTooShort", errTokenTooShort)

	// The decode direction, so newFromHash has a vector of its own. The subject
	// is the ascending token over the descending salt, the third case above.
	subjectRaw := append(goHash(asc[:], desc[:]), desc[:]...)
	subject := base64.StdEncoding.EncodeToString(subjectRaw)
	emit(sec, "kind=decoded_len hashed="+subject, fmt.Sprint(len(subjectRaw)))
	emit(sec, "kind=split_at hashed="+subject, fmt.Sprint(hashSize))
	split, err := newFromHash(subjectRaw)
	if err != nil {
		panic(err)
	}
	emit(sec, "kind=split_hash hashed="+subject, fmt.Sprintf("%x", split.hash))
	emit(sec, "kind=split_salt hashed="+subject, fmt.Sprintf("%x", split.salt))
	emit(sec, "kind=split_hash_len hashed="+subject, fmt.Sprint(len(split.hash)))
	emit(sec, "kind=split_salt_len hashed="+subject, fmt.Sprint(len(split.salt)))

	// newFromHash at, below and above the only length it accepts.
	for _, n := range []int{47, 48, 49} {
		_, err := newFromHash(make([]byte, n))
		emit(sec, fmt.Sprintf("kind=newfromhash len=%d", n), errText(err))
	}

	// CompareHashAndToken end to end, matching and not matching.
	ascB64 := base64.StdEncoding.EncodeToString(asc[:])
	descB64 := base64.StdEncoding.EncodeToString(desc[:])
	emit(sec, "kind=compare token="+ascB64+" hashed="+subject,
		errText(compareHashAndToken(subject, ascB64)))
	emit(sec, "kind=compare token="+descB64+" hashed="+subject,
		errText(compareHashAndToken(subject, descB64)))

	// A 15 byte and a 17 byte token against the same subject hash. Neither
	// length is checked anywhere, so both come back as an ordinary mismatch and
	// not as one of the two token size errors above.
	short15 := base64.StdEncoding.EncodeToString(asc[:15])
	long17 := base64.StdEncoding.EncodeToString(append(append([]byte{}, asc[:]...), 0x10))
	emit(sec, "kind=compare token="+short15+" hashed="+subject,
		errText(compareHashAndToken(subject, short15)))
	emit(sec, "kind=compare token="+long17+" hashed="+subject,
		errText(compareHashAndToken(subject, long17)))

	// A token that is not base64 at all, which fails in the decode before any
	// of the hashing runs. The text is encoding/base64's, not wire-pod's.
	const undecodable = "!!!!"
	emit(sec, "kind=compare token="+undecodable+" hashed="+subject,
		errText(compareHashAndToken(subject, undecodable)))

	// The same for an undecodable hash, which is the argument that reaches this
	// function from disk rather than from the robot.
	emit(sec, "kind=compare token="+ascB64+" hashed="+undecodable,
		errText(compareHashAndToken(undecodable, ascB64)))
}

// errText renders an error the way this recording spells success, which is the
// literal "ok" rather than an empty field.
func errText(err error) string {
	if err == nil {
		return "ok"
	}
	return err.Error()
}

// ---------------------------------------------------------------- rfc3339

type rfcCase struct {
	sec  int64
	nsec int
	off  int
}

func rfc3339Section() {
	const sec = "rfc3339"

	// Every fraction shape, at one fixed instant in UTC. The layout's nines
	// trim trailing zeros, so the digit count in the output is the recording.
	const base = int64(1700000000) // 2023-11-14T22:13:20Z
	fractions := []int{
		0,         // no fraction at all, and no decimal point either
		1,         // 9 digits, eight of them leading zeros
		10,        // 8 digits
		100,       // 7 digits
		1000,      // 6 digits
		10000,     // 5 digits
		100000,    // 4 digits
		1000000,   // 3 digits
		10000000,  // 2 digits
		100000000, // 1 digit
		120000000,
		123000000,
		123400000,
		123450000,
		123456000,
		123456700,
		123456780,
		123456789,
		500000000,
		900000000,
		999999900,
		999999990,
		999999999,
	}
	for _, ns := range fractions {
		emitRFC(sec, rfcCase{sec: base, nsec: ns, off: 0})
	}

	// Every offset shape, at three fraction lengths.
	offsets := []int{
		0,      // UTC, which the layout writes as "Z"
		-25200, // -07:00
		19800,  // +05:30, a half hour offset
		7200,   // +02:00, a whole hour positive
		-1800,  // -00:30, an offset smaller than an hour
		34200,  // +09:30
	}
	for _, off := range offsets {
		for _, ns := range []int{0, 123456789, 500000000} {
			emitRFC(sec, rfcCase{sec: base, nsec: ns, off: off})
		}
	}

	// Edge instants: the epoch, one second before it, a century boundary, a
	// leap day, and the last second a four digit year can spell.
	for _, s := range []int64{0, -1, 946684800, 1709164800, 253402300799} {
		for _, off := range []int{0, -25200} {
			for _, ns := range []int{0, 123456789} {
				emitRFC(sec, rfcCase{sec: s, nsec: ns, off: off})
			}
		}
	}
}

func emitRFC(section string, c rfcCase) {
	// The zone name is never printed by RFC3339Nano, whose "Z07:00" writes "Z"
	// when the offset is zero and a signed hh:mm otherwise, so the offset is
	// the only input the zone contributes.
	zone := time.FixedZone("PROBE", c.off)
	got := time.Unix(c.sec, int64(c.nsec)).In(zone).Format(time.RFC3339Nano)
	emit(section, fmt.Sprintf("kind=format unix=%d nsec=%d off=%d", c.sec, c.nsec, c.off), got)
}

// --------------------------------------------------------------- addmonth

type dateCase struct {
	y                    int
	mo                   time.Month
	d, h, mi, s, ns, off int
}

func addMonthSection() {
	const sec = "addmonth"

	cases := []dateCase{
		// The end of every month of a common year.
		{y: 2023, mo: time.January, d: 31},
		{y: 2023, mo: time.February, d: 28},
		{y: 2023, mo: time.March, d: 31},
		{y: 2023, mo: time.April, d: 30},
		{y: 2023, mo: time.May, d: 31},
		{y: 2023, mo: time.June, d: 30},
		{y: 2023, mo: time.July, d: 31},
		{y: 2023, mo: time.August, d: 31},
		{y: 2023, mo: time.September, d: 30},
		{y: 2023, mo: time.October, d: 31},
		{y: 2023, mo: time.November, d: 30},
		{y: 2023, mo: time.December, d: 31}, // rolls the year over
		// The end of every month of a leap year.
		{y: 2024, mo: time.January, d: 31},
		{y: 2024, mo: time.February, d: 29},
		{y: 2024, mo: time.March, d: 31},
		{y: 2024, mo: time.April, d: 30},
		{y: 2024, mo: time.May, d: 31},
		{y: 2024, mo: time.June, d: 30},
		{y: 2024, mo: time.July, d: 31},
		{y: 2024, mo: time.August, d: 31},
		{y: 2024, mo: time.September, d: 30},
		{y: 2024, mo: time.October, d: 31},
		{y: 2024, mo: time.November, d: 30},
		{y: 2024, mo: time.December, d: 31},
		// The overflow cases spelled out: 29, 30 and 31 January in a common
		// year and in a leap year, which is where the two diverge.
		{y: 2023, mo: time.January, d: 29},
		{y: 2023, mo: time.January, d: 30},
		{y: 2024, mo: time.January, d: 29},
		{y: 2024, mo: time.January, d: 30},
		// The other 31 day months whose successor has 30 days.
		{y: 2023, mo: time.March, d: 30},
		{y: 2023, mo: time.August, d: 30},
		{y: 2023, mo: time.October, d: 30},
		// A century year that is not a leap year, and one that is.
		{y: 1900, mo: time.January, d: 30},
		{y: 2000, mo: time.January, d: 30},
		// Time of day and fraction ride along untouched.
		{y: 2023, mo: time.January, d: 31, h: 23, mi: 59, s: 59, ns: 999999999},
		{y: 2024, mo: time.February, d: 29, h: 12, mi: 34, s: 56, ns: 789000000},
		{y: 2026, mo: time.September, d: 9, h: 1, mi: 2, s: 3, ns: 4},
		// The same civil date under three fixed offsets. A time.FixedZone has
		// one offset for all time, so here the offset rides through untouched
		// and is never used to shift the date. That is a property of
		// time.FixedZone and not a general rule: the addmonth_local section
		// below records what a zone with transitions does instead, where the
		// offset printed on the result is the one in effect a month later.
		{y: 2023, mo: time.January, d: 31, h: 12, off: 0},
		{y: 2023, mo: time.January, d: 31, h: 12, off: -25200},
		{y: 2023, mo: time.January, d: 31, h: 12, off: 19800},
	}

	for _, c := range cases {
		zone := time.FixedZone("PROBE", c.off)
		in := time.Date(c.y, c.mo, c.d, c.h, c.mi, c.s, c.ns, zone)
		out := in.AddDate(0, 1, 0)
		emit(sec, fmt.Sprintf("kind=add_months y=%d mo=%d d=%d h=%d mi=%d s=%d ns=%d off=%d in=%s",
			c.y, int(c.mo), c.d, c.h, c.mi, c.s, c.ns, c.off, in.Format(time.RFC3339Nano)),
			out.Format(time.RFC3339Nano))
	}
}

// ---------------------------------------------------------- addmonth_local

// probeZone is the zone the addmonth_local cases run in. See the doc comment's
// "The zone the addmonth_local section uses" for why this one and why every
// instant below sits in 2026 or 2027.
const probeZone = "America/Los_Angeles"

// localCase is a civil date read in probeZone. There is no offset field: the
// whole point of this section is that the zone, not the case, decides the
// offset, and that it can decide a different one for the result than for the
// input.
type localCase struct {
	name            string
	y               int
	mo              time.Month
	d, h, mi, s, ns int
}

// addMonthLocalSection records AddDate(0, 1, 0) in a zone that has daylight
// saving transitions, which is what token.go:195-196 runs: currentTime and
// expiresAt both come from time.Now(), so their location is time.Local.
//
// Three lines are emitted per case, each carrying one scalar: the formatted
// result, its Unix second and its offset in seconds. Together with in_unix and
// in_off on the first line, that is everything a test needs to drive a fake
// clock whose offset function is keyed by Unix second, without a tz database of
// its own.
func addMonthLocalSection() {
	const sec = "addmonth_local"

	loc, err := time.LoadLocation(probeZone)
	if err != nil {
		panic(err)
	}

	emit(sec, "kind=const name=zone", probeZone)

	cases := []localCase{
		// A month that crosses no transition, so both ends carry -07:00.
		{name: "control_no_transition", y: 2026, mo: time.September, d: 9, h: 12},
		// A month that crosses the spring forward. The input is -08:00 and the
		// result is -07:00, which is the case an implementation that carries
		// the input's offset into the result gets wrong.
		{name: "crosses_spring_forward", y: 2026, mo: time.February, d: 9, h: 12},
		// The same with a fraction, to show it rides along untouched.
		{name: "crosses_spring_forward_frac", y: 2026, mo: time.February, d: 9, h: 12, ns: 123456789},
		// The day before, which crosses nothing, so both ends are -08:00.
		{name: "day_before_spring_forward", y: 2026, mo: time.February, d: 7, h: 12},
		// A month that crosses the fall back, -07:00 in and -08:00 out.
		{name: "crosses_fall_back", y: 2026, mo: time.October, d: 9, h: 12},
		// The target wall time does not exist: 8 March 02:30 is inside the hour
		// the spring forward removes. time.Date normalises backwards by the gap
		// rather than forwards, so the answer is 01:30 -08:00 and not 03:30
		// -07:00.
		{name: "lands_in_missing_hour", y: 2026, mo: time.February, d: 8, h: 2, mi: 30},
		// The target wall time happens twice: 1 November 01:30 is inside the
		// hour the fall back repeats. time.Date picks the first of the two,
		// which is the -07:00 one.
		{name: "lands_in_repeated_hour", y: 2026, mo: time.October, d: 1, h: 1, mi: 30},
		// The input itself is a wall time that does not exist, which is what a
		// hand written civil-to-instant conversion has to handle before AddDate
		// is even reached.
		{name: "starts_in_missing_hour", y: 2026, mo: time.March, d: 8, h: 2, mi: 30},
		// The input is inside the repeated hour.
		{name: "starts_in_repeated_hour", y: 2026, mo: time.November, d: 1, h: 1, mi: 30},
		// Month ends, where the day overflow and a transition happen together.
		{name: "month_end_crosses_spring_forward", y: 2026, mo: time.February, d: 28, h: 12},
		{name: "month_end_crosses_fall_back", y: 2026, mo: time.October, d: 31, h: 12},
		// 31 January in a real zone: the day overflows to 3 March, still
		// before the transition, so the offset is unchanged.
		{name: "month_end_overflow", y: 2026, mo: time.January, d: 31, h: 12},
		// The year roll, with no transition anywhere near it.
		{name: "year_roll", y: 2026, mo: time.December, d: 31, h: 12},
	}

	for _, c := range cases {
		in := time.Date(c.y, c.mo, c.d, c.h, c.mi, c.s, c.ns, loc)
		out := in.AddDate(0, 1, 0)
		_, inOff := in.Zone()
		_, outOff := out.Zone()
		key := fmt.Sprintf("case=%s zone=%s y=%d mo=%d d=%d h=%d mi=%d s=%d ns=%d",
			c.name, probeZone, c.y, int(c.mo), c.d, c.h, c.mi, c.s, c.ns)
		emit(sec, fmt.Sprintf("kind=add_months_local %s in_unix=%d in_off=%d in=%s",
			key, in.Unix(), inOff, in.Format(time.RFC3339Nano)),
			out.Format(time.RFC3339Nano))
		emit(sec, "kind=out_unix case="+c.name, fmt.Sprint(out.Unix()))
		emit(sec, "kind=out_off case="+c.name, fmt.Sprint(outOff))
	}

	// The transition instants themselves, so the offset function a test builds
	// from this section has its step edges pinned rather than inferred from the
	// cases above. Each pair is the last second of the old offset and the first
	// second of the new one. They are written in UTC because a local wall time
	// at a transition is either missing or ambiguous, which is the very thing
	// under test.
	for _, u := range []time.Time{
		// Spring forward 2026: at 10:00 UTC, PST becomes PDT.
		time.Date(2026, time.March, 8, 9, 59, 59, 0, time.UTC),
		time.Date(2026, time.March, 8, 10, 0, 0, 0, time.UTC),
		// Fall back 2026: at 09:00 UTC, PDT becomes PST.
		time.Date(2026, time.November, 1, 8, 59, 59, 0, time.UTC),
		time.Date(2026, time.November, 1, 9, 0, 0, 0, time.UTC),
		// Spring forward 2027, one year on. The cases above do not reach it;
		// it is here so the recording shows the rule repeating rather than a
		// pair of edges that could be read as one-off constants.
		time.Date(2027, time.March, 14, 9, 59, 59, 0, time.UTC),
		time.Date(2027, time.March, 14, 10, 0, 0, 0, time.UTC),
	} {
		t := u.In(loc)
		_, off := t.Zone()
		emit(sec, fmt.Sprintf("kind=offset zone=%s unix=%d", probeZone, t.Unix()),
			fmt.Sprint(off))
	}
}

// ---------------------------------------------------------------- f32json

// f32Doc is the shape config.go:38-39 has: a float32 behind a json tag. The
// value has to reach encoding/json as a float32 struct field, not as a bare
// float64, or floatEncoder would be asked for 64 bits and 0.7 would widen.
type f32Doc struct {
	V float32 `json:"v"`
}

type f32Case struct {
	expr string
	val  float32
}

func f32JSONSection() {
	const sec = "f32json"

	cases := []f32Case{
		{"float32(0)", 0},
		{"float32(math.Copysign(0,-1))", float32(math.Copysign(0, -1))},
		{"float32(1)", 1},
		{"float32(-1)", -1},
		{"float32(0.7)", 0.7}, // the top_p default
		{"float32(-0.7)", -0.7},
		{"float32(0.1)", 0.1},
		{"float32(0.5)", 0.5},
		{"float32(0.9)", 0.9},
		{"float32(1.5)", 1.5},
		{"float32(2.5)", 2.5},
		{"float32(0.05)", 0.05},
		{"float32(0.123456789)", 0.123456789},
		// An exact decimal tie. 0x3ee90000 is 0.455078125 exactly, nine
		// significant digits, and eight digits round-trip, so the shortest
		// form sits exactly halfway between 0.45507812 and 0.45507813.
		// strconv breaks that half to even: ryuDigits32
		// (strconv/ftoaryu.go:412, the round-up flag at :456-461) rounds an
		// exact half up only when the truncation is odd. A formatter that
		// rounds a half up unconditionally, which is what Rust does, writes
		// the other digit here. Every float32 tie has a magnitude between
		// 2^-12 and 2^22, so a tie always reaches encoding/json through the
		// plain form and the exponent form can never carry one.
		{"math.Float32frombits(0x3ee90000)", math.Float32frombits(0x3ee90000)},
		// The small end cutoff. encode.go:557 compares float32(abs) < 1e-6, so
		// the boundary is float32(1e-6) itself: at it the format stays 'f', and
		// one ulp below it flips to 'e'.
		{"math.Nextafter32(float32(1e-6),0)", math.Nextafter32(float32(1e-6), 0)},
		{"float32(1e-6)", 1e-6},
		{"math.Nextafter32(float32(1e-6),1)", math.Nextafter32(float32(1e-6), 1)},
		{"float32(1e-7)", 1e-7},
		// The exponent cleanup at encode.go:562-569 rewrites e-09 to e-9 but
		// leaves e-10 alone, and never touches a '+' exponent.
		{"float32(1e-9)", 1e-9},
		{"float32(1.5e-9)", 1.5e-9},
		{"float32(9.99e-9)", 9.99e-9},
		{"float32(1e-10)", 1e-10},
		{"float32(5e-10)", 5e-10},
		{"float32(-1e-9)", -1e-9},
		// The large end cutoff, abs >= 1e21.
		{"float32(1e20)", 1e20},
		{"math.Nextafter32(float32(1e21),0)", math.Nextafter32(float32(1e21), 0)},
		{"float32(1e21)", 1e21},
		{
			"math.Nextafter32(float32(1e21),float32(math.MaxFloat32))",
			math.Nextafter32(float32(1e21), float32(math.MaxFloat32)),
		},
		{"float32(1e22)", 1e22},
		// The ends of the type.
		{"float32(math.MaxFloat32)", math.MaxFloat32},
		{"float32(-math.MaxFloat32)", -math.MaxFloat32},
		{"math.Float32frombits(0x00800000)", math.Float32frombits(0x00800000)}, // smallest normal
		{"math.Float32frombits(0x00400000)", math.Float32frombits(0x00400000)}, // a subnormal
		{"float32(math.SmallestNonzeroFloat32)", math.SmallestNonzeroFloat32},
		// Rejected by encoding/json. The bit patterns are written out rather
		// than derived from math.NaN() so the recorded input does not depend on
		// how a float64 NaN narrows on the host's FPU.
		{"math.Float32frombits(0x7fc00000)", math.Float32frombits(0x7fc00000)}, // quiet NaN
		{"math.Float32frombits(0x7f800000)", math.Float32frombits(0x7f800000)}, // +Inf
		{"math.Float32frombits(0xff800000)", math.Float32frombits(0xff800000)}, // -Inf
	}

	for _, c := range cases {
		in := fmt.Sprintf("v=0x%08x expr=%s", math.Float32bits(c.val), c.expr)
		b, err := json.Marshal(f32Doc{V: c.val})
		if err != nil {
			emit(sec, "kind=error "+in, err.Error())
			continue
		}
		s := string(b)
		// The struct marshals to exactly {"v":<number>}, so the number is the
		// slice between the fixed five byte prefix and the closing brace.
		// Assert that rather than trust it.
		const prefix = `{"v":`
		if !strings.HasPrefix(s, prefix) || !strings.HasSuffix(s, "}") {
			panic("unexpected marshalling of f32Doc: " + s)
		}
		emit(sec, "kind=struct "+in, s)
		emit(sec, "kind=number "+in, s[len(prefix):len(s)-1])
	}
}

// ----------------------------------------------------------------- claims

func claimsSection() {
	const sec = "claims"

	// Fixed placeholders. None of these is a live value: the requestor id
	// carries an all zero ESN, the token id is an all zero UUID with the
	// version and variant nibbles a v4 would have, and the two timestamps are
	// one calendar month apart in a fixed zone.
	const (
		iat         = "2026-09-09T12:34:56.789012345-07:00"
		expires     = "2026-10-09T12:34:56.789012345-07:00"
		requestorID = "vic:00000000"
		tokenID     = "00000000-0000-4000-8000-000000000000"
		tokenType   = "user+robot"
		userID      = "wirepod"
	)

	claims := jwt.MapClaims{
		"expires":      expires,
		"iat":          iat,
		"permissions":  nil,
		"requestor_id": requestorID,
		"token_id":     tokenID,
		"token_type":   tokenType,
		"user_id":      userID,
	}
	token := jwt.NewWithClaims(jwt.SigningMethodRS512, claims)

	header, err := json.Marshal(token.Header)
	if err != nil {
		panic(err)
	}
	payload, err := json.Marshal(token.Claims)
	if err != nil {
		panic(err)
	}

	emit(sec, "kind=header", string(header))
	emit(sec, "kind=payload", string(payload))
	emit(sec, "kind=header_b64url", jwt.EncodeSegment(header))
	emit(sec, "kind=payload_b64url", jwt.EncodeSegment(payload))
	emit(sec, "kind=signing_input", jwt.EncodeSegment(header)+"."+jwt.EncodeSegment(payload))
	emit(sec, "kind=alg", jwt.SigningMethodRS512.Alg())
	emit(sec, "kind=key_order",
		"expires,iat,permissions,requestor_id,token_id,token_type,user_id")

	// The inputs, so the Rust side can rebuild the same claim set instead of
	// reading them back out of the payload it is trying to verify.
	emit(sec, "kind=value name=expires", expires)
	emit(sec, "kind=value name=iat", iat)
	emit(sec, "kind=value name=permissions", "null")
	emit(sec, "kind=value name=requestor_id", requestorID)
	emit(sec, "kind=value name=token_id", tokenID)
	emit(sec, "kind=value name=token_type", tokenType)
	emit(sec, "kind=value name=user_id", userID)

	// The two literals the Go server hard codes, token.go:31 and token.go:187.
	emit(sec, "kind=const name=UserId", "wirepod")
	emit(sec, "kind=const name=default_requestor_id", "vic:00601b50")
}

// ------------------------------------------------------------ legacystamp

// legacyLine is logger.go:102-111 verbatim.
func legacyLine(now time.Time, comp string, bot string, msg string) string {
	s := now.Format("2006.01.02 15:04:05") + ": "
	if comp != "" {
		s = s + "[" + comp + "] "
	}
	if bot != "" {
		s = s + bot + ": "
	}
	return s + msg + "\n"
}

// fileFormatLine is logger.go:113-122 verbatim, with the Level.String call
// replaced by a caller supplied string; logger.go:20-31 is that mapping and it
// is recorded separately below.
func fileFormatLine(now time.Time, level string, comp string, bot string, msg string) string {
	s := now.Format("2006.01.02 15:04:05") + " " + level + " "
	if comp != "" {
		s = s + "[" + comp + "] "
	}
	if bot != "" {
		s = s + bot + ": "
	}
	return s + msg + "\n"
}

func legacyStampSection() {
	const sec = "legacystamp"

	stamps := []dateCase{
		// Single digit month, day, hour, minute and second, all zero padded.
		{y: 2026, mo: time.January, d: 2, h: 3, mi: 4, s: 5},
		// Every field two digits already.
		{y: 2026, mo: time.December, d: 31, h: 23, mi: 59, s: 59},
		// Midnight, where the hour is 00 and not 24 or 12.
		{y: 2026, mo: time.September, d: 9, h: 0, mi: 0, s: 0},
		// Noon, where a 12 hour layout would be wrong.
		{y: 2026, mo: time.September, d: 9, h: 12, mi: 0, s: 0},
		// A nanosecond value, which this layout drops entirely.
		{y: 2026, mo: time.September, d: 9, h: 12, mi: 0, s: 0, ns: 987654321},
		// A non-zero offset, which this layout also drops: the stamp is the
		// wall clock reading in whatever zone the time carries.
		{y: 2026, mo: time.September, d: 9, h: 7, mi: 6, s: 5, off: -25200},
		{y: 1999, mo: time.November, d: 5, h: 8, mi: 9, s: 1},
	}
	for _, c := range stamps {
		zone := time.FixedZone("PROBE", c.off)
		t := time.Date(c.y, c.mo, c.d, c.h, c.mi, c.s, c.ns, zone)
		emit(sec, fmt.Sprintf("kind=stamp y=%d mo=%d d=%d h=%d mi=%d s=%d ns=%d off=%d",
			c.y, int(c.mo), c.d, c.h, c.mi, c.s, c.ns, c.off),
			t.Format("2006.01.02 15:04:05"))
	}

	// The two whole line layouts, with the component and bot fields present and
	// absent. The bot is an all zero placeholder, never a live ESN.
	lineAt := time.Date(2026, time.January, 2, 3, 4, 5, 0, time.FixedZone("PROBE", 0))
	type lineCase struct {
		level, comp, bot, msg string
	}
	lines := []lineCase{
		{"INFO", "", "", "hello"},
		{"INFO", "mdns", "", "hello"},
		{"INFO", "", "00000000", "hello"},
		{"INFO", "token", "00000000", "hello"},
		{"DEBUG", "jdocs", "00000000", "hello"},
		{"WARN", "web", "", "hello"},
		{"ERROR", "conn", "00000000", "hello"},
	}
	for _, c := range lines {
		in := fmt.Sprintf("comp=%s bot=%s msg=%s", c.comp, c.bot, c.msg)
		emit(sec, "kind=legacy_line "+in, legacyLine(lineAt, c.comp, c.bot, c.msg))
		emit(sec, fmt.Sprintf("kind=file_line level=%s %s", c.level, in),
			fileFormatLine(lineAt, c.level, c.comp, c.bot, c.msg))
	}

	// logger.go:20-31: anything that is not INFO, WARN or ERROR is DEBUG.
	emit(sec, "kind=level_string level=0", "DEBUG")
	emit(sec, "kind=level_string level=1", "INFO")
	emit(sec, "kind=level_string level=2", "WARN")
	emit(sec, "kind=level_string level=3", "ERROR")
	emit(sec, "kind=level_string level=4", "DEBUG")
	emit(sec, "kind=const name=stamp_layout", "2006.01.02 15:04:05")
}
