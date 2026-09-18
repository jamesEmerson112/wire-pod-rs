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
//	claims_matrix
//	             pkg/servers/token/token.go:254-265 again, driven end to end
//	             over fourteen instants, five requestor ids and three token
//	             ids, in America/Los_Angeles and in UTC. The claims section
//	             above records one claim set and exists to pin the header, the
//	             key order and the two segments; this one exists to pin what
//	             moves. See "How the claims_matrix section is built" below.
//	jws          pkg/servers/token/token.go:266-267, rsa.GenerateKey at 1024
//	             bits and SignedString, recorded as invariants only: the
//	             segment count, the signature's byte and character lengths,
//	             that the first two segments are the signing string untouched,
//	             that no standard-alphabet byte reaches a token, the two header
//	             values, and which of two signings differ. No key and no
//	             signature byte is written. See "Why the jws section records no
//	             signature" below.
//	robot_parse  The robot's own reader, not the server's writer:
//	             vector-cloud/internal/token/identity/identity.go:158
//	             (ParseUnverified) followed by
//	             vector-cloud/internal/token/identity/token.go:96-161
//	             (FromJwtToken), over thirty-one crafted tokens. See "The
//	             module the robot_parse section substitutes" below.
//	uuid         github.com/google/uuid v1.6.0's version4.go:47
//	             (NewRandomFromReader), which is the transform uuid.New
//	             (version4.go:13) runs over the sixteen bytes it draws, and
//	             which pkg/servers/token/token.go:180-183 turns into the
//	             token_id claim.
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
// # How the claims_matrix section is built
//
// The same library and the same seven claims, driven over what the claims
// section holds fixed. Fourteen cases move three things at once: the instant,
// which sits on both sides of two daylight saving transitions and inside both
// the hour a spring forward removes and the hour a fall back repeats; the
// requestor id, which covers the default serial, a lowercase serial, an
// uppercase one, one carrying all five characters encoding/json escapes, and
// one whose bytes force base64url groups 62 and 63 so EncodeSegment itself has
// to produce '-' and '_'; and the token id, which takes the three UUIDs the
// Rust tests already pin uuid_v4 against.
//
// Five lines per case, because a test needs more than the payload: the expires
// string, its Unix second and its offset are what let a driver check the
// instant rather than only the formatting, and the signing input is the two
// encoded segments. The payload line's input column carries the instant in
// every form the driver needs, so it never has to own a tz database. The six
// kind=offset lines are the same transition edges addmonth_local records,
// repeated because a driver rebuilt from them has to answer for the two extra
// instants a whole claim build asks about.
//
// The requestor ids travel as lowercase hex of their UTF-8 bytes rather than
// as themselves, because two of the five carry bytes no recording should hold
// literally: U+2028 and U+2029 are line separators, and a run of '?' and '~'
// is unreadable. Every byte this program prints stays printable ASCII.
//
// # Why the jws section records no signature
//
// token.go:266-267 generates a throwaway 1024-bit RSA key per request and
// signs with it. The key is a local variable and nothing stores or publishes
// it, so no two runs of this program could agree on a signature even if one
// were worth recording. What is worth recording is the shape around it, and
// every line in that section is an invariant: a count, a length, a header
// value, or a yes-or-no over two signings. Two throwaway keys are generated on
// every run and the section's output is identical regardless.
//
// The one answer that is not obvious is same_key_twice_differs. PKCS#1 v1.5 is
// deterministic, so the same key over the same signing input produces the same
// signature: it is the per-request key, not the signing, that makes two tokens
// issued in the same second differ.
//
// A toolchain that refuses a 1024-bit key panics rather than falling back to a
// larger one, because the signature's length is the contract being recorded
// and a larger key would record a different one under the same name. Go 1.24
// is the first release to impose a floor and it sits at exactly 1024.
//
// # The module the robot_parse section substitutes
//
// That section is the robot's reader, and the robot builds against
// github.com/dgrijalva/jwt-go v3.2.1-0.20180719211823-0b96aaa70776+incompatible
// (vector-cloud/go.mod:8), which is not in this machine's module cache. This
// program runs github.com/golang-jwt/jwt v3.2.2+incompatible instead, the
// maintained fork of the same code at the same major version and the version
// the Go server itself pins (chipper/go.mod:16). Both module paths are
// recorded as consts so the substitution is on the record rather than in a
// comment.
//
// One case is known to be able to differ between them. padded_payload_segment
// is golang-jwt's verdict: its DecodeSegment is base64.RawURLEncoding, which
// refuses a '=' outright, while the older dgrijalva build re-pads the segment
// before decoding and may accept it. Nothing this port writes is padded, so
// the difference is unreachable from a token wire-pod issues.
//
// No error text from encoding/json, time or encoding/base64 is recorded, only
// a closed verdict vocabulary, because those strings move with the Go
// toolchain and nothing on the wire carries them: the robot only branches on
// whether the parse failed. The three verdicts that are literals are literals,
// verbatim: "signing method (alg) is unavailable." and "signing method (alg)
// is unspecified." are parser.go:141 and :144 in the jwt module,
// "tokenstring should not contain 'bearer '" is parser.go:108, and
// "missing claim " is identity/token.go:167-169.
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
	"bytes"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"strings"
	"time"
	_ "time/tzdata"

	"github.com/golang-jwt/jwt"
	"github.com/google/uuid"
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
	claimsMatrixSection()
	jwsSection()
	robotParseSection()
	uuidSection()
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
		// A near tie, which is the other half of the same rule. 0x00000060
		// is 96 * 2^-149, whose exact expansion is
		// 1.345246525751824388086780399958e-43, so the digit after the
		// shortest three is a 5 and the digits after that one are not all
		// zeros. That makes the value strictly nearer 1.35e-43 than
		// 1.34e-43 rather than halfway between them, and strconv writes
		// 1.35e-43: the round-up flag in ryuDigits32 (ftoaryu.go:456-461)
		// is set whenever the trimmed tail is non-zero, without consulting
		// parity at all. A tie breaker that read the 5 and not the tail
		// would call this a tie, break it to the even 1.34e-43, and still
		// pass the exact-tie case above.
		{"math.Float32frombits(0x00000060)", math.Float32frombits(0x00000060)},
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

// ----------------------------------------------------------- claims_matrix

// The zones the matrix runs in. The first is the zone addmonth_local already
// uses and this section reuses its six transition edges, so a test can rebuild
// one step function and drive both. The second is the degenerate case: an
// offset of zero, which time.RFC3339Nano writes as "Z" rather than "+00:00".
const (
	matrixZone    = "America/Los_Angeles"
	matrixZoneUTC = "UTC"
)

// matrixRequestor is one value of the requestor_id claim, under the label the
// cases name it by. The id is the whole claim value, "vic:" included, because
// token.go:187 and token.go:223 are the only two things that build it and both
// produce the complete string.
type matrixRequestor struct {
	label string
	id    string
}

// matrixTokenID is one value of the token_id claim. All three are the UUIDs
// crates/wirepod-core/tests/jwt.rs already pins uuid_v4 against, so the matrix
// adds no new placeholder.
type matrixTokenID struct {
	label string
	id    string
}

// matrixCase is one whole CreateJWT claim build: a civil instant read in a
// named zone, a requestor and a token id.
type matrixCase struct {
	name            string
	zone            string
	y               int
	mo              time.Month
	d, h, mi, s, ns int
	requestor       string
	tokenID         string
}

// claimsMatrixSection drives token.go:254-265 end to end over a matrix of
// instants, requestors and token ids, which the claims section above does not:
// that one records a single claim set and exists to pin the header, the key
// order and the two base64url segments.
//
// Five lines per case. The payload line carries the instant in every form a
// test needs (the civil fields, the Unix second, the offset and the formatted
// iat) so the driver never has to own a tz database; the other four carry the
// expires string, its Unix second, its offset and the signing input.
func claimsMatrixSection() {
	const sec = "claims_matrix"

	la, err := time.LoadLocation(matrixZone)
	if err != nil {
		panic(err)
	}
	utc, err := time.LoadLocation(matrixZoneUTC)
	if err != nil {
		panic(err)
	}

	emit(sec, "kind=const name=zone", matrixZone)
	emit(sec, "kind=const name=zone_utc", matrixZoneUTC)

	// The same six transition edges addmonth_local records, repeated here so
	// this section stands on its own: Claims::new asks the zone for an offset
	// at two more instants than AddDate does, so a driver built from these six
	// samples has to cover both.
	for _, u := range []time.Time{
		time.Date(2026, time.March, 8, 9, 59, 59, 0, time.UTC),
		time.Date(2026, time.March, 8, 10, 0, 0, 0, time.UTC),
		time.Date(2026, time.November, 1, 8, 59, 59, 0, time.UTC),
		time.Date(2026, time.November, 1, 9, 0, 0, 0, time.UTC),
		time.Date(2027, time.March, 14, 9, 59, 59, 0, time.UTC),
		time.Date(2027, time.March, 14, 10, 0, 0, 0, time.UTC),
	} {
		t := u.In(la)
		_, off := t.Zone()
		emit(sec, fmt.Sprintf("kind=offset zone=%s unix=%d", matrixZone, t.Unix()),
			fmt.Sprint(off))
	}

	requestors := []matrixRequestor{
		// token.go:187, the serial a first authentication claims.
		{"unknown", "vic:00601b50"},
		// token.go:223 with a lowercase placeholder serial.
		{"robot_lower", "vic:00000000"},
		// token.go:223 concatenates the serial untouched, so a robot whose
		// bot-info "thing" carried uppercase hex claims uppercase hex.
		{"robot_upper", "vic:0000ABCD"},
		// The five characters encoding/json escapes and encoding/json alone.
		// '<', '&' and '>' come out as \u003c, \u0026 and \u003e, and
		// U+2028 and U+2029 as \u2028 and \u2029; a serde_json payload would
		// carry all five raw. The serial is written with \u escapes below so that
		// no byte of this source file is outside printable ASCII.
		{"robot_escapes", "vic:<&>\u2028\u2029"},
		// '?' is 0x3f and '~' is 0x7e, the two ASCII bytes whose runs force
		// six-bit groups 63 and 62, so jwt.EncodeSegment itself has to produce
		// '_' and '-' rather than the standard alphabet's '/' and '+'. Eight of
		// each covers every byte alignment the surrounding payload can impose.
		{"robot_alphabet", "vic:????????~~~~~~~~"},
	}
	byRequestor := map[string]string{}
	for _, r := range requestors {
		byRequestor[r.label] = r.id
		emit(sec, "kind=requestor_hex name="+r.label, hex.EncodeToString([]byte(r.id)))
	}

	tokenIDs := []matrixTokenID{
		{"zero", "00000000-0000-4000-8000-000000000000"},
		{"max", "ffffffff-ffff-4fff-bfff-ffffffffffff"},
		{"mixed", "00112233-4455-4677-8899-aabbccddeeff"},
	}
	byTokenID := map[string]string{}
	for _, t := range tokenIDs {
		byTokenID[t.label] = t.id
		emit(sec, "kind=token_id name="+t.label, t.id)
	}

	cases := []matrixCase{
		// Five controls at one instant that crosses no transition, one per
		// requestor, so the requestor is the only thing that moves.
		{name: "control_unknown", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "unknown", tokenID: "zero"},
		{name: "control_lower", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "robot_lower", tokenID: "mixed"},
		{name: "control_upper", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "robot_upper", tokenID: "max"},
		{name: "control_escapes", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "robot_escapes", tokenID: "zero"},
		{name: "control_alphabet", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "robot_alphabet", tokenID: "zero"},
		// iat is -08:00 and expires, one month later, is -07:00.
		{name: "spring_forward", zone: matrixZone, y: 2026, mo: time.February, d: 9, h: 12, ns: 123456789, requestor: "unknown", tokenID: "mixed"},
		// The other direction, with a fraction that loses eight trailing zeros.
		{name: "fall_back", zone: matrixZone, y: 2026, mo: time.October, d: 9, h: 12, ns: 500000000, requestor: "unknown", tokenID: "max"},
		// expires lands in the hour the spring forward removed.
		{name: "missing_hour", zone: matrixZone, y: 2026, mo: time.February, d: 8, h: 2, mi: 30, requestor: "robot_lower", tokenID: "zero"},
		// expires lands in the hour the fall back repeated.
		{name: "repeated_hour", zone: matrixZone, y: 2026, mo: time.October, d: 1, h: 1, mi: 30, requestor: "robot_lower", tokenID: "zero"},
		// 31 January, which normalises forward into March.
		{name: "month_end_overflow", zone: matrixZone, y: 2026, mo: time.January, d: 31, h: 12, requestor: "robot_upper", tokenID: "mixed"},
		// The year roll, at the longest fraction RFC3339Nano writes.
		{name: "year_roll", zone: matrixZone, y: 2026, mo: time.December, d: 31, h: 12, ns: 999999999, requestor: "unknown", tokenID: "zero"},
		// A fraction of exactly zero, which drops the decimal point entirely.
		{name: "zero_fraction", zone: matrixZone, y: 2026, mo: time.September, d: 9, h: 12, requestor: "unknown", tokenID: "zero"},
		// Offset zero, which RFC3339Nano writes as "Z" and not "+00:00".
		{name: "utc_control", zone: matrixZoneUTC, y: 2026, mo: time.September, d: 9, h: 12, requestor: "unknown", tokenID: "zero"},
		{name: "utc_fraction", zone: matrixZoneUTC, y: 2026, mo: time.September, d: 9, h: 12, ns: 123456789, requestor: "robot_lower", tokenID: "mixed"},
	}

	for _, c := range cases {
		loc := la
		if c.zone == matrixZoneUTC {
			loc = utc
		}
		// token.go:195-196: both instants come from time.Now() in time.Local,
		// and the second is the first plus one calendar month.
		in := time.Date(c.y, c.mo, c.d, c.h, c.mi, c.s, c.ns, loc)
		out := in.AddDate(0, 1, 0)
		_, inOff := in.Zone()
		_, outOff := out.Zone()
		iat := in.Format(time.RFC3339Nano)
		expires := out.Format(time.RFC3339Nano)

		requestorID, ok := byRequestor[c.requestor]
		if !ok {
			panic("no requestor labelled " + c.requestor)
		}
		tokenID, ok := byTokenID[c.tokenID]
		if !ok {
			panic("no token id labelled " + c.tokenID)
		}

		// token.go:254-265 verbatim, with the two hard-coded literals from
		// token.go:31 and token.go:263.
		token := jwt.NewWithClaims(jwt.SigningMethodRS512, jwt.MapClaims{
			"expires":      expires,
			"iat":          iat,
			"permissions":  nil,
			"requestor_id": requestorID,
			"token_id":     tokenID,
			"token_type":   "user+robot",
			"user_id":      "wirepod",
		})
		payload, err := json.Marshal(token.Claims)
		if err != nil {
			panic(err)
		}
		signing, err := token.SigningString()
		if err != nil {
			panic(err)
		}
		if c.requestor == "robot_alphabet" {
			segment := signing[strings.Index(signing, ".")+1:]
			if !strings.Contains(segment, "-") || !strings.Contains(segment, "_") {
				panic("the alphabet requestor no longer forces base64url groups 62 and 63")
			}
		}

		emit(sec, fmt.Sprintf(
			"kind=payload case=%s requestor=%s token_id=%s zone=%s y=%d mo=%d d=%d h=%d mi=%d s=%d ns=%d iat_unix=%d iat_off=%d iat=%s",
			c.name, c.requestor, c.tokenID, c.zone, c.y, int(c.mo), c.d, c.h, c.mi, c.s,
			c.ns, in.Unix(), inOff, iat),
			string(payload))
		emit(sec, "kind=expires case="+c.name, expires)
		emit(sec, "kind=exp_unix case="+c.name, fmt.Sprint(out.Unix()))
		emit(sec, "kind=exp_off case="+c.name, fmt.Sprint(outOff))
		emit(sec, "kind=signing_input case="+c.name, signing)
	}
}

// -------------------------------------------------------------------- jws

// jwsKeyBits is the RSA key size token.go:266 asks rsa.GenerateKey for.
const jwsKeyBits = 1024

// jwsSection records what SignedString's assembly looks like from the outside,
// and nothing else: no key, no signature and no byte of either is written.
//
// Two throwaway keys are generated on every run and thrown away when it ends,
// which is what the Go server does per request. Everything emitted below is an
// invariant over their output: a count, a length, or a yes-or-no, so the
// recording is stable across runs even though the keys are not.
func jwsSection() {
	const sec = "jws"

	// The same fixed placeholder claim set the claims section uses. Nothing
	// here is a live value.
	claims := jwt.MapClaims{
		"expires":      "2026-10-09T12:34:56.789012345-07:00",
		"iat":          "2026-09-09T12:34:56.789012345-07:00",
		"permissions":  nil,
		"requestor_id": "vic:00000000",
		"token_id":     "00000000-0000-4000-8000-000000000000",
		"token_type":   "user+robot",
		"user_id":      "wirepod",
	}

	// A refusal is fatal rather than a smaller key: the length of the
	// signature slot is the thing being recorded, so a different size would
	// record a different contract under the same name. Go 1.24 is the first
	// release to impose a floor at all and it sits at exactly 1024.
	generate := func() *rsa.PrivateKey {
		key, err := rsa.GenerateKey(rand.Reader, jwsKeyBits)
		if err != nil {
			panic(fmt.Sprintf(
				"this toolchain refuses the %d-bit key token.go:266 generates: %v",
				jwsKeyBits, err))
		}
		return key
	}

	sign := func(key *rsa.PrivateKey) (string, string) {
		token := jwt.NewWithClaims(jwt.SigningMethodRS512, claims)
		input, err := token.SigningString()
		if err != nil {
			panic(err)
		}
		signed, err := token.SignedString(key)
		if err != nil {
			panic(err)
		}
		return input, signed
	}

	first := generate()
	second := generate()
	signingInput, firstToken := sign(first)
	_, firstAgain := sign(first)
	_, secondToken := sign(second)

	parts := strings.Split(firstToken, ".")
	if len(parts) != 3 {
		panic("SignedString did not produce three segments")
	}
	signature, err := jwt.DecodeSegment(parts[2])
	if err != nil {
		panic(err)
	}

	header := jwt.NewWithClaims(jwt.SigningMethodRS512, claims).Header
	alg, ok := header["alg"].(string)
	if !ok {
		panic("the library's header carries no string alg")
	}
	typ, ok := header["typ"].(string)
	if !ok {
		panic("the library's header carries no string typ")
	}

	emit(sec, "kind=const name=key_bits", fmt.Sprint(jwsKeyBits))
	emit(sec, "kind=segments", fmt.Sprint(len(parts)))
	emit(sec, "kind=sig_chars", fmt.Sprint(len(parts[2])))
	emit(sec, "kind=sig_bytes", fmt.Sprint(len(signature)))
	// token.go:65-83 in the library: SignedString is SigningString plus a dot
	// plus the encoded signature, so the first two segments are untouched.
	emit(sec, "kind=head_is_signing_input",
		fmt.Sprint(parts[0]+"."+parts[1] == signingInput))
	// EncodeSegment is RawURLEncoding, so no '+', '/' or '=' can reach a token
	// however the signature bytes fall.
	emit(sec, "kind=sig_has_no_standard_alphabet",
		fmt.Sprint(!strings.ContainsAny(parts[2], "+/=")))
	emit(sec, "kind=alg_header", alg)
	emit(sec, "kind=typ_header", typ)
	// PKCS#1 v1.5 is deterministic: the same key over the same signing input
	// produces the same signature, so a per-request key is the only thing that
	// makes two tokens issued in the same second differ.
	emit(sec, "kind=same_key_twice_differs", fmt.Sprint(firstToken != firstAgain))
	emit(sec, "kind=fresh_key_each_call_differs", fmt.Sprint(firstToken != secondToken))
}

// ------------------------------------------------------------- robot_parse

// The closed verdict vocabulary. No error text from encoding/json, time or
// encoding/base64 is recorded, because those move with the Go toolchain and
// nothing on the wire carries them; the robot only ever branches on whether the
// parse failed. The three that are library or vector-cloud literals are exactly
// that, verbatim: the two alg strings are parser.go:141 and :144 in
// github.com/golang-jwt/jwt v3.2.2, the bearer string is parser.go:108, and
// "missing claim " is vector-cloud/internal/token/identity/token.go:167-169.
const (
	verdictOK           = "ok"
	verdictSegments     = "segments"
	verdictBase64       = "base64 error"
	verdictHeaderJSON   = "header not json"
	verdictClaimsObject = "claims not object"
	verdictTimeParse    = "time parse error"
	verdictBearer       = "tokenstring should not contain 'bearer '"
	verdictAlgUnavail   = "signing method (alg) is unavailable."
	verdictAlgUnspec    = "signing method (alg) is unspecified."
)

// robotMissingClaim is identity/token.go:167-169.
func robotMissingClaim(claim string) string {
	return fmt.Sprintf("missing claim %s", claim)
}

// robotParseUnverified is
// github.com/golang-jwt/jwt@v3.2.2/parser.go:96-149, the call
// vector-cloud/internal/token/identity/identity.go:158 makes, with each error
// replaced by its verdict and the parsed claims handed back on success.
//
// The signature segment is never decoded, which is the whole reason the Rust
// port can put random bytes there.
func robotParseUnverified(tokenString string) (jwt.MapClaims, string) {
	parts := strings.Split(tokenString, ".")
	if len(parts) != 3 {
		return nil, verdictSegments
	}
	headerBytes, err := jwt.DecodeSegment(parts[0])
	if err != nil {
		if strings.HasPrefix(strings.ToLower(tokenString), "bearer ") {
			return nil, verdictBearer
		}
		return nil, verdictBase64
	}
	header := map[string]interface{}{}
	if err := json.Unmarshal(headerBytes, &header); err != nil {
		return nil, verdictHeaderJSON
	}
	claimBytes, err := jwt.DecodeSegment(parts[1])
	if err != nil {
		return nil, verdictBase64
	}
	claims := jwt.MapClaims{}
	if err := json.NewDecoder(bytes.NewBuffer(claimBytes)).Decode(&claims); err != nil {
		return nil, verdictClaimsObject
	}
	if method, ok := header["alg"].(string); ok {
		if jwt.GetSigningMethod(method) == nil {
			return nil, verdictAlgUnavail
		}
	} else {
		return nil, verdictAlgUnspec
	}
	return claims, verdictOK
}

// robotFromJwtToken is
// vector-cloud/internal/token/identity/token.go:96-161, in the order that file
// reads the claims, reduced to a verdict. The nil-token and non-MapClaims arms
// (:98 and :160) cannot be reached from identity.go:158, which always passes a
// parsed token holding a jwt.MapClaims, so they have no case here.
func robotFromJwtToken(claims jwt.MapClaims) string {
	// :103, :108, :113 and :118, in that order. Every one is a type assertion
	// to string, so a claim that is present but is a JSON number, object or
	// null fails the same way a missing one does.
	for _, name := range []string{"token_id", "token_type", "user_id", "requestor_id"} {
		if _, ok := claims[name].(string); !ok {
			return robotMissingClaim(name)
		}
	}
	// :123-129.
	issuedAt, ok := claims["iat"].(string)
	if !ok {
		return robotMissingClaim("iat")
	}
	if _, err := time.ParseInLocation(time.RFC3339, issuedAt, time.UTC); err != nil {
		return verdictTimeParse
	}
	// :131-138.
	expiresAt, ok := claims["expires"].(string)
	if !ok {
		return robotMissingClaim("expires")
	}
	if _, err := time.ParseInLocation(time.RFC3339, expiresAt, time.UTC); err != nil {
		return verdictTimeParse
	}
	// :153-156: permissions is optional and only a JSON object populates it.
	// Null, an array and a string all leave the field nil and none is an error.
	_, _ = claims["permissions"].(map[string]interface{})
	return verdictOK
}

// robotParse is identity.go:157-167 (parseToken): ParseUnverified, then
// FromJwtToken.
func robotParse(tokenString string) string {
	claims, verdict := robotParseUnverified(tokenString)
	if verdict != verdictOK {
		return verdict
	}
	return robotFromJwtToken(claims)
}

// robotCase is one crafted token and the name it is recorded under.
type robotCase struct {
	name  string
	token string
}

// robotParseSection runs the robot's own token reader over a matrix of crafted
// tokens and records what it answers.
//
// Every token here is built in this program out of the fixed placeholder claim
// set with jwt.EncodeSegment. None is signed, and the signature segment is a
// fixed placeholder string, because nothing in the robot's path looks at it.
//
// The module substitution is on record as a const. The robot builds against
// github.com/dgrijalva/jwt-go v3.2.1-0.20180719211823-0b96aaa70776+incompatible
// (vector-cloud/go.mod:8), which is not in this machine's module cache; the Go
// server and this program use github.com/golang-jwt/jwt v3.2.2+incompatible
// (chipper/go.mod:16), the maintained fork of the same code at the same major
// version.
func robotParseSection() {
	const sec = "robot_parse"

	emit(sec, "kind=const name=required_claims",
		"token_id,token_type,user_id,requestor_id,iat,expires")
	emit(sec, "kind=const name=optional_claims", "permissions")
	emit(sec, "kind=const name=parse_layout", time.RFC3339)
	emit(sec, "kind=const name=jwt_module", "github.com/golang-jwt/jwt v3.2.2+incompatible")
	emit(sec, "kind=const name=robot_module",
		"github.com/dgrijalva/jwt-go v3.2.1-0.20180719211823-0b96aaa70776+incompatible")

	base := func() map[string]interface{} {
		return map[string]interface{}{
			"expires":      "2026-10-09T12:34:56.789012345-07:00",
			"iat":          "2026-09-09T12:34:56.789012345-07:00",
			"permissions":  nil,
			"requestor_id": "vic:00000000",
			"token_id":     "00000000-0000-4000-8000-000000000000",
			"token_type":   "user+robot",
			"user_id":      "wirepod",
		}
	}
	payloadOf := func(mutate func(map[string]interface{})) []byte {
		claims := base()
		if mutate != nil {
			mutate(claims)
		}
		out, err := json.Marshal(claims)
		if err != nil {
			panic(err)
		}
		return out
	}
	headerOf := func(text string) []byte { return []byte(text) }

	defaultHeader, err := json.Marshal(
		jwt.NewWithClaims(jwt.SigningMethodRS512, jwt.MapClaims{}).Header)
	if err != nil {
		panic(err)
	}
	headerSeg := jwt.EncodeSegment(defaultHeader)
	validPayload := payloadOf(nil)
	payloadSeg := jwt.EncodeSegment(validPayload)
	// Never a real signature: the robot's path never decodes this segment.
	signatureSeg := jwt.EncodeSegment([]byte("probe-placeholder"))

	assemble := func(header, payload []byte) string {
		return jwt.EncodeSegment(header) + "." + jwt.EncodeSegment(payload) + "." + signatureSeg
	}
	withClaims := func(mutate func(map[string]interface{})) string {
		return assemble(defaultHeader, payloadOf(mutate))
	}
	withIAT := func(value string) string {
		return withClaims(func(c map[string]interface{}) { c["iat"] = value })
	}
	drop := func(name string) string {
		return withClaims(func(c map[string]interface{}) { delete(c, name) })
	}

	// The payload encoded by the padding encoder rather than the raw one.
	// base64.RawURLEncoding rejects '=' outright, which is the difference the
	// case is here for; if the fixed payload's length happened to be a multiple
	// of three the padded encoder would emit no '=' at all, so a single
	// insignificant trailing space is added in that case. It is the same JSON
	// value either way, and the decode never gets far enough to see it.
	padded := validPayload
	if len(padded)%3 == 0 {
		padded = append(append([]byte{}, padded...), ' ')
	}
	paddedSeg := base64.URLEncoding.EncodeToString(padded)
	if !strings.Contains(paddedSeg, "=") {
		panic("the padded payload segment carries no padding, so the case proves nothing")
	}

	valid := assemble(defaultHeader, validPayload)

	cases := []robotCase{
		{"valid", valid},

		// token.go:103, :108, :113, :118, :123 and :131: a claim that is not a
		// string, missing being one way to not be one.
		{"missing_token_id", drop("token_id")},
		{"missing_token_type", drop("token_type")},
		{"missing_user_id", drop("user_id")},
		{"missing_requestor_id", drop("requestor_id")},
		{"missing_iat", drop("iat")},
		{"missing_expires", drop("expires")},
		// A JSON number decodes to a float64, which fails the same assertion,
		// so the robot cannot read a numeric-timestamp token however standard
		// that spelling is elsewhere.
		{"numeric_iat", withClaims(func(c map[string]interface{}) { c["iat"] = 1789000496 })},
		{"numeric_expires", withClaims(func(c map[string]interface{}) { c["expires"] = 1791592496 })},

		// token.go:153-156: only an object populates permissions, and nothing
		// else there is an error.
		{"null_permissions", withClaims(func(c map[string]interface{}) { c["permissions"] = nil })},
		{"object_permissions", withClaims(func(c map[string]interface{}) {
			c["permissions"] = map[string]interface{}{"robot": true}
		})},
		{"array_permissions", withClaims(func(c map[string]interface{}) {
			c["permissions"] = []interface{}{"robot"}
		})},

		// An empty user_id parses. What it costs is one level up: identity.go
		// :141-145 deletes the token file at boot when it sees one.
		{"empty_user_id", withClaims(func(c map[string]interface{}) { c["user_id"] = "" })},

		// time.RFC3339 as ParseInLocation reads it (token.go:126, :135): a
		// fraction of any length is accepted even though the layout has none,
		// and both zero-offset spellings are accepted.
		{"iat_fraction_0", withIAT("2026-09-09T12:34:56-07:00")},
		{"iat_fraction_3", withIAT("2026-09-09T12:34:56.789-07:00")},
		{"iat_fraction_9", withIAT("2026-09-09T12:34:56.789012345-07:00")},
		{"iat_zulu", withIAT("2026-09-09T12:34:56Z")},
		{"iat_plus_zero_offset", withIAT("2026-09-09T12:34:56+00:00")},
		// The two shapes it refuses: no zone at all, and a space where the
		// layout's literal 'T' is.
		{"iat_no_zone", withIAT("2026-09-09T12:34:56")},
		{"iat_space_separator", withIAT("2026-09-09 12:34:56Z")},

		// parser.go:97-100, strings.Split on '.' and then an exact count.
		{"two_segments", headerSeg + "." + payloadSeg},
		{"four_segments", valid + ".extra"},

		// The signature segment is never decoded, which is what makes this
		// port's random-bytes slot safe.
		{"empty_signature_segment", headerSeg + "." + payloadSeg + "."},
		{"garbage_signature_segment", headerSeg + "." + payloadSeg + "." + "this-is-not-a-signature"},

		// parser.go:120-122 through DecodeSegment, which is RawURLEncoding and
		// refuses padding outright.
		{"padded_payload_segment", headerSeg + "." + paddedSeg + "." + signatureSeg},

		// parser.go:139-145, the alg lookup.
		{"unknown_alg", assemble(headerOf(`{"alg":"RS999","typ":"JWT"}`), validPayload)},
		{"missing_alg", assemble(headerOf(`{"typ":"JWT"}`), validPayload)},
		{"alg_none", assemble(headerOf(`{"alg":"none","typ":"JWT"}`), validPayload)},

		// parser.go:112-114 and :123-136: the header has to unmarshal into a
		// map and the payload has to decode into one.
		{"header_not_json", assemble(headerOf(`[1,2]`), validPayload)},
		{"payload_not_object", headerSeg + "." + jwt.EncodeSegment([]byte(`[1,2]`)) + "." + signatureSeg},

		// parser.go:107-108, the one error message that names the caller's
		// mistake rather than the library's.
		{"bearer_prefix", "bearer " + valid},
	}

	for _, c := range cases {
		emit(sec, "kind=verdict name="+c.name, robotParse(c.token))
	}
}

// ------------------------------------------------------------------- uuid

// uuidSection records github.com/google/uuid's transform from sixteen drawn
// bytes to the string token.go:180-183 puts in the token_id claim.
//
// uuid.New (version4.go:13) is Must(NewRandom()), NewRandom reads sixteen bytes
// from crypto/rand and hands them to NewRandomFromReader (version4.go:47),
// which is what runs below over a fixed draw instead. The draw decides
// everything except the version nibble and the two variant bits, which that
// function overwrites.
func uuidSection() {
	const sec = "uuid"

	emit(sec, "kind=const name=module", "github.com/google/uuid v1.6.0")
	emit(sec, "kind=const name=layout", "8-4-4-4-12")

	for _, draw := range []string{
		// Every bit clear and every bit set, so both overwrites are visible.
		"00000000000000000000000000000000",
		"ffffffffffffffffffffffffffffffff",
		// An ascending pattern, which shows that nothing outside bytes 6 and 8
		// is touched and that the hex is lowercase.
		"00112233445566778899aabbccddeeff",
		// A descending nibble pattern. Byte 8 is 0x87, whose top two bits are
		// already 10, so the variant overwrite is a no-op here while the
		// version overwrite still turns byte 6 from 0x69 into 0x49: the two
		// are separate rules and this draw exercises one of them alone.
		"0f1e2d3c4b5a69788796a5b4c3d2e1f0",
		// Alternating words, so a transform that read the bytes in the wrong
		// order would show up.
		"ffffffff00000000ffffffff00000000",
	} {
		raw, err := hex.DecodeString(draw)
		if err != nil {
			panic(err)
		}
		id, err := uuid.NewRandomFromReader(bytes.NewReader(raw))
		if err != nil {
			panic(err)
		}
		emit(sec, "kind=uuid draw="+draw, id.String())
	}
}
