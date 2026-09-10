// Command ini-probe records the exact bytes gopkg.in/ini.v1 writes for the two
// shapes the Go server's WriteToIniPrimary produces, so the hand written Rust
// writer that replaces the library in Phase 1 has the output to match rather
// than a guess about it.
//
// # What is reproduced
//
// pkg/servers/jdocs/botInfoStorer.go:31-62 in the Go checkout's chipper/
// directory. That function loads ~/.anki_vector/sdk_config.ini, falls back to
// ini.Empty() when the load fails, then takes one of two paths:
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
// It then calls SaveTo, :60. SaveTo is SaveToIndent(filename, "") which builds
// the same buffer WriteTo builds and hands it to os.WriteFile
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
//   - KeyValueDelimiterOnWrite is "=" by default (ini.go:114-115).
//   - Values are only quoted when they need it: a value containing a newline or
//     a backtick is wrapped in triple quotes, a value containing '#' or ';' is
//     wrapped in backticks, and a value with leading or trailing whitespace is
//     wrapped in double quotes (file.go:461-468). A Windows path full of
//     backslashes needs none of that and is written through untouched, which is
//     why the cert values below carry backslashes on purpose.
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
	emit(sec, "kind=setting name=LineBreak", ini.LineBreak)
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
	// filled in with "=" (ini.go:114-115), so it is recorded as the literal the
	// library defaults to rather than read back off a File.
	emit(sec, "kind=setting name=KeyValueDelimiterOnWrite", "=")
	emit(sec, "kind=setting name=delimiter_written", " = ")

	// The two key orders botInfoStorer.go uses, recorded because they differ.
	emit(sec, "kind=setting name=create_key_order", "cert,ip,name,guid")
	emit(sec, "kind=setting name=update_key_order", "cert,name,ip,guid")
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
}
