// Command ini-probe records the exact bytes gopkg.in/ini.v1 writes for the four
// shapes the Go server's two sdk_config.ini writers produce, so the hand written
// Rust writer that replaces the library in Phase 1 has the output to match
// rather than a guess about it.
//
// # What is reproduced
//
// pkg/servers/jdocs/botInfoStorer.go in the Go checkout's chipper/ directory
// holds two writers, and Phase 1 has to implement both. Each loads
// ~/.anki_vector/sdk_config.ini, falls back to ini.Empty() when the load fails,
// takes an update path or a create path, and ends in SaveTo.
//
// WriteToIniPrimary, :31-62, is called from jdocs/server.go:124 on the primary
// auth path:
//
//   - The update path, :39-48. It walks every section, and for a section whose
//     name matches the ESN case insensitively it calls Key(...).SetValue(...)
//     for cert, then name, then ip, then guid. Key() returns the existing key
//     when there is one and appends a new key when there is not, so an existing
//     section keeps the key order it was parsed with and only genuinely new
//     keys land at the end, in that call order.
//   - The create path, :49-59. It calls NewSection(esn) and then NewKey for
//     cert, then ip, then name, then guid. Note the order differs from the
//     update path: cert, ip, name, guid here against cert, name, ip, guid
//     above. That difference is visible in the file and is reproduced here.
//
// WriteToIniSecondary, :66-128, is called from jdocs/server.go:140 inside the
// brand new robot branch of ReadDocs, and neither of its paths matches the
// primary's:
//
//   - The update path, :78-89, sets guid and then ip and touches neither cert
//     nor name. Two keys, not four, and in an order the primary never uses. A
//     section missing guid and ip therefore gets them appended in that order,
//     which is what kind=secondary_update_missing_keys records.
//   - The create path, :115-125, calls NewKey for cert, ip, name, guid. That
//     happens to be the primary create order, but it is a separate call site,
//     so it is recorded separately rather than assumed to stay in step.
//
// Both writers end in SaveTo, :60 and :127. SaveTo is SaveToIndent(filename,
// "") which builds the same buffer WriteTo builds and hands it to os.WriteFile
// (file.go:525-540 in the library), so the bytes this program takes from
// WriteTo are the bytes the Go server puts on disk.
//
// The library version is gopkg.in/ini.v1 v1.67.3, the version pinned by the Go
// checkout's chipper/go.mod, so this recording is authoritative for the server
// that is running today.
//
// # How the library shapes the output
//
// From ini.go:30-56 and file.go:335-506 of v1.67.3:
//
//   - PrettyFormat is true, so every key inside a section is padded with spaces
//     out to the length of the longest key name in that section, and the
//     delimiter is written as " = " with a space on each side
//     (file.go:335-340 and :456-459). The padding is per section, not per file.
//   - PrettySection is true, so one bare line break separates sections, and the
//     last section does not get a trailing one (file.go:498-503). The file
//     therefore ends with exactly one line break, the one after the last key.
//   - DefaultHeader is false and the DEFAULT section is written only when it
//     holds keys (file.go:363-372), so a file of named sections has no header.
//   - KeyValueDelimiterOnWrite is a per-load option whose zero value is filled
//     in with "=" by newFile at file.go:54-55. ini.go:114-115 is the field and
//     the doc comment that states the same default in prose.
//   - Values are only quoted when they need it, in the three arms at
//     file.go:461-468: a value containing a newline or a backtick is wrapped in
//     triple quotes, a value containing '#' or ';' is wrapped in backticks
//     unless IgnoreInlineComment is set, and a value with leading or trailing
//     whitespace is wrapped in double quotes. A Windows path full of
//     backslashes trips none of the three and is written through untouched,
//     which is why the cert values below carry backslashes on purpose. The four
//     kind=quote_* cases record each arm firing rather than leaving the rule as
//     a claim, because the bot name reaches both the name key and the middle of
//     the cert path and is not a literal the server controls: on the primary
//     path it comes from the caller at botInfoStorer.go:44 and on the secondary
//     path from the session certificate's Issuer CommonName at :105.
//
// # The line break, and why this program pins it
//
// ini.go:35-37 declares LineBreak as "\n" and ini.go:60-64 rewrites it to
// "\r\n" in an init function when runtime.GOOS is "windows" and the process is
// not a test binary. wire-pod runs on Windows here, so the file on disk is CRLF
// terminated. This program assigns ini.LineBreak = "\r\n" explicitly before it
// writes anything, which is a no-op on Windows and makes the recording identical
// on a Linux or macOS host, so the committed expected.txt is a property of the
// library and not of whoever regenerated it.
//
// Because of that assignment, the setting line for it is named
// line_break_written rather than LineBreak: it reads back this program's own
// store, so it is a statement about the file wire-pod writes on Windows and not
// about the library's default. The default is recorded beside it as
// line_break_default, taken from the declaration rather than from a read.
//
// # Nothing here is a live value
//
// The ESNs, bot names, IP addresses, GUID strings and the SDK directory are all
// fixed placeholders written into this file. No path, address or identifier is
// read from this machine or from the running server.
//
// # Output format
//
// Identical to the go-probe recording beside this one. One case per line, three
// tab separated fields:
//
//	<section>\t<input>\t<output>
//
// Field 2 is space separated key=value pairs whose first pair is always
// kind=<what the line records>; no value contains a space or a tab. Field 3 is
// ALWAYS a Go %q quoted string literal, which is what makes the CRLF pairs and
// the alignment spaces in the recorded file bytes visible and exact. Lines
// beginning with '#' are comments and there are no blank lines. The pair
// (field 1, field 2) is unique across the file.
//
// There are two sections:
//
//	setting  one library knob per line, the ones that shape the output.
//	file     the complete bytes of one written file per line.
//
// # Regenerating
//
//	cargo run -p xtask -- go-probe            rewrites expected.txt
//	cargo run -p xtask -- go-probe --check     verifies it without rewriting
package main

import (
	"bytes"
	"fmt"
	"strings"

	"gopkg.in/ini.v1"
)

// Fixed placeholders. The trailing separator is part of the value because
// botInfoStorer.go concatenates vars.SDKIniPath with the file name directly.
const sdkIniPath = `C:\probe\.anki_vector\`

func emit(section, input, output string) {
	fmt.Printf("%s\t%s\t%q\n", section, input, output)
}

func main() {
	// See the doc comment: pinned so the recording does not depend on the host
	// operating system. On Windows, where wire-pod runs, this is what
	// ini.go:60-64 has already done.
	ini.LineBreak = "\r\n"

	fmt.Println("# Recorded stdout of docs/phases/P1-robot-connect-auth/ini-probe/main.go.")
	fmt.Println("# One case per line: <section>TAB<input>TAB<output>. Field 2 is space separated")
	fmt.Println("# key=value pairs beginning with kind=. Field 3 is ALWAYS a Go %q quoted string")
	fmt.Println("# literal, so the CRLF pairs and the alignment spaces are exact. main.go's doc")
	fmt.Println("# comment names the Go source and the library version this reproduces.")
	fmt.Println("# Regenerate with: cargo run -p xtask -- go-probe")
	fmt.Println("# Verify with:     cargo run -p xtask -- go-probe --check")

	settingsSection()
	fileSection()
}

// -------------------------------------------------------------- settings

func settingsSection() {
	const sec = "setting"

	emit(sec, "kind=setting name=module", "gopkg.in/ini.v1 v1.67.3")

	// Read back off the package var this program assigned at the top of main,
	// so it records the byte pair wire-pod writes on Windows rather than the
	// library's default. That default is the literal from the declaration at
	// ini.go:35-37, which the init at ini.go:60-64 overwrites on Windows.
	emit(sec, "kind=setting name=line_break_written", ini.LineBreak)
	emit(sec, "kind=setting name=line_break_default", "\n")
	emit(sec, "kind=setting name=DefaultSection", ini.DefaultSection)
	emit(sec, "kind=setting name=DefaultHeader", fmt.Sprint(ini.DefaultHeader))
	emit(sec, "kind=setting name=PrettySection", fmt.Sprint(ini.PrettySection))
	emit(sec, "kind=setting name=PrettyFormat", fmt.Sprint(ini.PrettyFormat))
	emit(sec, "kind=setting name=PrettyEqual", fmt.Sprint(ini.PrettyEqual))
	emit(sec, "kind=setting name=DefaultFormatLeft", ini.DefaultFormatLeft)
	emit(sec, "kind=setting name=DefaultFormatRight", ini.DefaultFormatRight)

	// The delimiter that actually reaches the file. file.go:335-340 starts from
	// DefaultFormatLeft + KeyValueDelimiterOnWrite + DefaultFormatRight and
	// then, because PrettyFormat is true, replaces it with the space padded
	// form. KeyValueDelimiterOnWrite is a per-load option whose zero value is
	// filled in with "=" by newFile at file.go:54-55, so it is recorded as the
	// literal that fill-in uses rather than read back off a File.
	emit(sec, "kind=setting name=KeyValueDelimiterOnWrite", "=")
	emit(sec, "kind=setting name=delimiter_written", " = ")

	// The four key orders botInfoStorer.go uses. All four are recorded, and
	// each name carries the writer it belongs to, so nobody reads one pair as
	// the only pair. The two update orders are the ones that differ: the
	// primary writes four keys and the secondary writes two.
	emit(sec, "kind=setting name=primary_create_key_order", "cert,ip,name,guid")
	emit(sec, "kind=setting name=primary_update_key_order", "cert,name,ip,guid")
	emit(sec, "kind=setting name=secondary_create_key_order", "cert,ip,name,guid")
	emit(sec, "kind=setting name=secondary_update_key_order", "guid,ip")
}

// ------------------------------------------------------------------ file

// primaryEdit is one WriteToIniPrimary call: the arguments the Go server passes
// at botInfoStorer.go:31.
type primaryEdit struct {
	botName string
	esn     string
	guid    string
	ip      string
}

// writeToIniPrimary is botInfoStorer.go:31-62 with the disk removed. The load
// is from a byte slice instead of a path, the mkdir and the log lines are gone,
// and SaveTo becomes WriteTo, which builds the identical buffer
// (file.go:509-540). Everything that decides bytes is unchanged.
func writeToIniPrimary(existing []byte, edits []primaryEdit) string {
	var f *ini.File
	if existing == nil {
		f = ini.Empty()
	} else {
		var err error
		f, err = ini.Load(existing)
		if err != nil {
			panic(err)
		}
	}

	for _, e := range edits {
		matched := false
		for _, section := range f.Sections() {
			if strings.EqualFold(section.Name(), e.esn) {
				matched = true
				section.Key("cert").SetValue(sdkIniPath + e.botName + "-" + e.esn + ".cert")
				section.Key("name").SetValue(e.botName)
				section.Key("ip").SetValue(e.ip)
				section.Key("guid").SetValue(e.guid)
			}
		}
		if !matched {
			newSection, err := f.NewSection(e.esn)
			if err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("cert", sdkIniPath+e.botName+"-"+e.esn+".cert"); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("ip", e.ip); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("name", e.botName); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("guid", e.guid); err != nil {
				panic(err)
			}
		}
	}

	var buf bytes.Buffer
	if _, err := f.WriteTo(&buf); err != nil {
		panic(err)
	}
	return buf.String()
}

// secondaryEdit is one WriteToIniSecondary call. The Go signature is
// (esn, guid, ip); botName and certPath are the two values that function
// derives inside itself, from the matched section's name key on the update path
// and from the downloaded session certificate on the create path. They are
// passed in here because this program makes no network call, exactly as
// primaryEdit passes botName in.
type secondaryEdit struct {
	esn      string
	guid     string
	ip       string
	botName  string // create path only
	certPath string // create path only
}

// writeToIniSecondary is botInfoStorer.go:66-128 with the disk and the network
// removed. The load is from a byte slice, the mkdir, the log lines and the
// https://session-certs.token.global.anki-services.com fetch at :91-111 are
// gone, and SaveTo becomes WriteTo. Everything that decides bytes is unchanged,
// including the one thing that is not the primary writer's: the update path at
// :86-87 sets guid and then ip and leaves cert and name alone, where the
// primary sets all four.
//
// One hazard is deliberately not exercised. At :81-82 the real function does
// botNameKey, _ := section.GetKey("name") and then botNameKey.String(), so a
// matched section with no name key hands a nil *ini.Key to a method that
// dereferences it (key.go:179-181) and the Go server panics. The Rust port has
// to decide what to do there; no case below reaches it, because a recording
// cannot hold a panic.
func writeToIniSecondary(existing []byte, edits []secondaryEdit) string {
	var f *ini.File
	if existing == nil {
		f = ini.Empty()
	} else {
		var err error
		f, err = ini.Load(existing)
		if err != nil {
			panic(err)
		}
	}

	for _, e := range edits {
		certExists := false
		for _, section := range f.Sections() {
			if strings.EqualFold(section.Name(), e.esn) {
				certExists = true
				section.Key("guid").SetValue(e.guid)
				section.Key("ip").SetValue(e.ip)
			}
		}
		if !certExists {
			newSection, err := f.NewSection(e.esn)
			if err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("cert", e.certPath); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("ip", e.ip); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("name", e.botName); err != nil {
				panic(err)
			}
			if _, err := newSection.NewKey("guid", e.guid); err != nil {
				panic(err)
			}
		}
	}

	var buf bytes.Buffer
	if _, err := f.WriteTo(&buf); err != nil {
		panic(err)
	}
	return buf.String()
}

func fileSection() {
	const sec = "file"

	// Fixed placeholder identities. The GUIDs are base64 of 16 zero-ish bytes
	// chosen here, not values from any robot.
	alpha := primaryEdit{
		botName: "Vector-A1B2",
		esn:     "00000001",
		guid:    "AAECAwQFBgcICQoLDA0ODw==",
		ip:      "192.0.2.11",
	}
	beta := primaryEdit{
		botName: "Vector-C3D4",
		esn:     "00000002",
		guid:    "Dw4NDAsKCQgHBgUEAwIBAA==",
		ip:      "192.0.2.12",
	}
	alphaMoved := primaryEdit{
		botName: "Vector-Z9Y8",
		esn:     "00000001",
		guid:    "Wl1gY2ZpbG9ydXh7foGEhw==",
		ip:      "192.0.2.99",
	}

	// 1. A fresh file with one section: the create path from ini.Empty().
	one := writeToIniPrimary(nil, []primaryEdit{alpha})
	emit(sec, "kind=create sections=1", one)

	// 2. A fresh file with two sections, written by two create path calls
	//    against the same File, which is what two associations in a row do.
	two := writeToIniPrimary(nil, []primaryEdit{alpha, beta})
	emit(sec, "kind=create sections=2", two)

	// 3. An existing file whose section is updated: the update path, where all
	//    four keys already exist so the file's own key order survives and only
	//    the values change.
	emit(sec, "kind=update sections=1",
		writeToIniPrimary([]byte(one), []primaryEdit{alphaMoved}))

	// 4. The same update against a two section file, to show the untouched
	//    section is rewritten byte for byte and the blank line between sections
	//    is the library's, not the parsed file's.
	emit(sec, "kind=update sections=2",
		writeToIniPrimary([]byte(two), []primaryEdit{alphaMoved}))

	// 5. An existing file carrying a section this server never writes and a key
	//    this server never writes, both of which have to survive the rewrite.
	//    The unknown section also carries a much longer key name, which shows
	//    the alignment padding is computed per section.
	unknown := "[00000001]\r\n" +
		"cert  = " + sdkIniPath + "Vector-A1B2-00000001.cert\r\n" +
		"ip    = 192.0.2.11\r\n" +
		"name  = Vector-A1B2\r\n" +
		"guid  = AAECAwQFBgcICQoLDA0ODw==\r\n" +
		"extra = kept by the fork\r\n" +
		"\r\n" +
		"[some-other-tool]\r\n" +
		"a_very_long_key_name = 1\r\n" +
		"b                    = 2\r\n"
	emit(sec, "kind=update_unknown_survives sections=2",
		writeToIniPrimary([]byte(unknown), []primaryEdit{alphaMoved}))

	// 6. An existing section that is missing three of the four keys, so the
	//    update path appends them in its own call order, cert then name then ip
	//    then guid, after the key that was already there.
	partial := "[00000001]\r\n" +
		"name = Vector-A1B2\r\n"
	emit(sec, "kind=update_partial_section sections=1",
		writeToIniPrimary([]byte(partial), []primaryEdit{alphaMoved}))

	// 7. A create against a file that already holds an unrelated section, which
	//    is the ordinary second-robot case: the new section is appended after
	//    the existing one.
	emit(sec, "kind=create_after_existing sections=2",
		writeToIniPrimary([]byte(one), []primaryEdit{beta}))

	// ---- WriteToIniSecondary, botInfoStorer.go:66-128 -------------------

	alphaSecondary := secondaryEdit{
		esn:      "00000001",
		guid:     "Wl1gY2ZpbG9ydXh7foGEhw==",
		ip:       "192.0.2.99",
		botName:  "Vector-Z9Y8",
		certPath: sdkIniPath + "Vector-Z9Y8-00000001.cert",
	}
	betaSecondary := secondaryEdit{
		esn:      "00000002",
		guid:     "Dw4NDAsKCQgHBgUEAwIBAA==",
		ip:       "192.0.2.12",
		botName:  "Vector-C3D4",
		certPath: sdkIniPath + "Vector-C3D4-00000002.cert",
	}

	// 8. The secondary update path over a section that already holds all four
	//    keys. Only guid and ip change; cert and name keep the values the file
	//    was parsed with, which is the difference from the primary update.
	emit(sec, "kind=secondary_update_all_keys sections=1",
		writeToIniSecondary([]byte(one), []secondaryEdit{alphaSecondary}))

	// 9. The secondary update path over a section that has cert and name but
	//    neither guid nor ip, so the two are appended in the call order at
	//    :86-87, guid then ip. A writer that reused the primary's order would
	//    append ip then guid and pass every other case in this file.
	secondaryPartial := "[00000001]\r\n" +
		"cert = " + sdkIniPath + "Vector-A1B2-00000001.cert\r\n" +
		"name = Vector-A1B2\r\n"
	emit(sec, "kind=secondary_update_missing_keys sections=1",
		writeToIniSecondary([]byte(secondaryPartial), []secondaryEdit{alphaSecondary}))

	// 10. The secondary create path from an empty file, :115-125.
	emit(sec, "kind=secondary_create sections=1",
		writeToIniSecondary(nil, []secondaryEdit{alphaSecondary}))

	// 11. The secondary create path against a file that already holds another
	//     robot's section.
	emit(sec, "kind=secondary_create_after_existing sections=2",
		writeToIniSecondary([]byte(one), []secondaryEdit{betaSecondary}))

	// ---- The three value quoting arms at file.go:461-468 ----------------
	//
	// Every value in the cases above avoids all three triggers, so a writer
	// that skipped quoting entirely would pass all of them. The four cases
	// below fire all three arms, the middle arm through both of its trigger
	// characters. The trigger rides in on the bot name, which lands both in the
	// name key and in the middle of the cert path, so each case also records
	// which of the two keys ends up quoted.

	quoted := func(kind, botName string) {
		emit(sec, kind+" sections=1", writeToIniPrimary(nil, []primaryEdit{{
			botName: botName,
			esn:     "00000001",
			guid:    "AAECAwQFBgcICQoLDA0ODw==",
			ip:      "192.0.2.11",
		}}))
	}

	// Arm 1, :462-463: a backtick wraps the value in triple quotes.
	quoted("kind=quote_backtick", "Vector-A`B2")
	// Arm 2, :464-465: '#' and ';' each wrap the value in backticks, because
	// IgnoreInlineComment is false.
	quoted("kind=quote_hash", "Vector-A#B2")
	quoted("kind=quote_semicolon", "Vector-A;B2")
	// Arm 3, :466-467: trailing whitespace wraps the value in double quotes.
	// Only the name key trips it; in the cert path the space is interior.
	quoted("kind=quote_trailing_space", "Vector-A1B2 ")
}
