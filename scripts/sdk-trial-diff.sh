#!/usr/bin/env bash
#
# sdk-trial-diff.sh - run the same low-side-effect requests against the Go
# server and the Rust sdk-trial server and diff what comes back.
#
# RUNBOOK-SDK-TRIAL.md is the procedure this belongs to. Read it first: this
# script talks to a real robot over two SDK connections at once, and one of the
# requests near the end disconnects the Go server's connection for three
# seconds.
#
# Environment:
#   GO_BASE   the Go server        (default http://localhost:8080)
#   RS_BASE   the Rust sdk-trial   (default http://127.0.0.1:18080)
#   SERIAL    the robot's serial   (default 00303f28)
#   TIMEOUT   per-request seconds  (default 20; disconnect alone takes 3)
#
# Pointing both bases at the Go server is the way to test the script itself.
# Everything should then report `match`, because both sides are the same
# process; anything that does not is a request whose answer is live rather than
# fixed, and those are the ones marked `observe`.
#
# What is compared: the status line, and the nine headers this surface can
# carry, and the body byte for byte. `Date` is printed as `<masked>` and never
# compared, because it differs on every request by construction. `rttMs` digits
# in a net_probe body are rewritten to `<n>` before the compare, for the same
# reason, and the raw figures are printed instead.
#
# What is never requested: `/api-sdk/get_sdk_settings`, which writes to the
# robot. `/api-sdk/get_sdk_info` is requested once, and only its first 40 bytes
# are read, with everything after `"global_guid":"` replaced by `<guid>`. No
# robot GUID is ever printed by this script.
#
# Exit status: 0 when every `compare` request matched, 1 otherwise. A request
# marked `observe` or `expected` never changes the exit status; the summary
# counts all four kinds.
#
# Tools used: curl, bash, sed, awk, diff, and sleep for the stim window.

set -u

GO_BASE="${GO_BASE:-http://localhost:8080}"
RS_BASE="${RS_BASE:-http://127.0.0.1:18080}"
SERIAL="${SERIAL:-00303f28}"
TIMEOUT="${TIMEOUT:-20}"

# The headers this surface can carry. Every one of them is compared, and one
# that is absent on both sides compares equal as `-`, which is deliberate: the
# file server's 404 is defined partly by the `Cache-Control` it does not carry.
FIELDS="content-type content-length x-content-type-options pragma expires cache-control access-control-allow-origin location"

# Display widths. The full value is printed on its own line whenever the two
# sides differ, so the clipping here never hides a mismatch.
LABEL_W=30
VALUE_W=36

WORK="${TMPDIR:-/tmp}/sdk-trial-diff.$$"
mkdir -p "$WORK" || exit 1
trap 'rm -rf "$WORK"' EXIT

n_match=0
n_diff=0
n_expected=0
n_observe=0
n_skip=0
step=0

# One header's value, or nothing when it is absent. The name is matched
# case-insensitively, which is what HTTP says and what lets the two servers
# spell a header differently without it counting as a difference.
hdr_value() {
	# Every function below declares its variables `local`. The stim window and
	# the field walk both count with a loop variable, and without `local` the
	# inner one silently ends the outer one after a single pass.
	awk -v want="$2" '
		{
			p = index($0, ":")
			if (p == 0) next
			name = tolower(substr($0, 1, p - 1))
			if (name != want) next
			v = substr($0, p + 1)
			sub(/^[ \t\r]+/, "", v)
			sub(/[ \t\r]+$/, "", v)
			print v
			exit
		}' "$1"
}

# A value clipped for the side-by-side columns.
clip() {
	awk -v s="$1" -v n="$2" '
		BEGIN {
			if (length(s) > n) printf "%s..", substr(s, 1, n - 2)
			else printf "%s", s
		}'
}

# A body rendered on one line, with its newlines shown as \n.
#
# A body that ends in a newline renders the same as one that does not. That is
# not a gap in the compare, which is done on the bytes; `content-length` is a
# compared field and is the row that tells the two apart.
show_body() {
	awk '{ if (NR > 1) printf "\\n"; printf "%s", $0 }' "$1"
}

# Everything about a body that varies between two runs of the same server.
normalise_body() {
	sed -e 's/"rttMs":-\{0,1\}[0-9][0-9.eE+-]*/"rttMs":<n>/g' "$1"
}

# Fetches one URL into $WORK/$tag.{b,h,f,t}.
#
# `.b` is the body as it arrived, `.h` the header block with the CRLFs stripped,
# `.f` the compared fields one per line, and `.t` curl's own total time.
capture() {
	local base="$1"
	local path="$2"
	local tag="$3"
	local rc v f
	curl -s -m "$TIMEOUT" \
		-o "$WORK/$tag.b" \
		-D "$WORK/$tag.hraw" \
		-w '%{time_total}' \
		"$base$path" >"$WORK/$tag.t" 2>/dev/null
	rc=$?
	: >"$WORK/$tag.h"
	: >"$WORK/$tag.f"
	if [ "$rc" -ne 0 ]; then
		# A refused connection or a timeout is a result, not a reason to stop.
		# It compares unequal to anything the other side answered, which is
		# exactly what should happen.
		: >"$WORK/$tag.b"
		: >"$WORK/$tag.n"
		printf 'status: curl-failed-%s\n' "$rc" >>"$WORK/$tag.f"
		for f in $FIELDS; do printf '%s: -\n' "$f" >>"$WORK/$tag.f"; done
		printf '0' >"$WORK/$tag.t"
		return 0
	fi
	sed 's/\r$//' "$WORK/$tag.hraw" >"$WORK/$tag.h"
	printf 'status: %s\n' "$(awk 'NR == 1 { print $2 }' "$WORK/$tag.h")" >>"$WORK/$tag.f"
	for f in $FIELDS; do
		v="$(hdr_value "$WORK/$tag.h" "$f")"
		printf '%s: %s\n' "$f" "${v:--}" >>"$WORK/$tag.f"
	done
	normalise_body "$WORK/$tag.b" >"$WORK/$tag.n"
	# A body the normaliser rewrote has a Content-Length that follows the digits
	# it rewrote: 127.855 is one byte longer than 18.956, and the first trial
	# reported every net_probe as a mismatch on that byte alone. Compare the
	# length of the normalised body instead, which still catches a body that
	# differs anywhere else.
	if ! cmp -s "$WORK/$tag.b" "$WORK/$tag.n"; then
		sed -i "s/^content-length: .*/content-length: <n>+$(wc -c <"$WORK/$tag.n" | tr -d ' ')/" "$WORK/$tag.f"
	fi
}

# Prints one field row, and returns 1 when the two sides differ.
row() {
	local name="$1"
	local left="$2"
	local right="$3"
	if [ "$left" = "$right" ]; then
		printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" \
			"$name" "$(clip "$left" "$VALUE_W")" "$(clip "$right" "$VALUE_W")"
		return 0
	fi
	printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" \
		"$name" "$(clip "$left" "$VALUE_W")" "$(clip "$right" "$VALUE_W")"
	printf "    != GO: %s\n" "$left"
	printf "    != RS: %s\n" "$right"
	return 1
}

# One request against both bases.
#
#   $1 kind: compare | observe | expected | go-only
#   $2 path
#   $3 note, printed under the heading; empty for none
request() {
	local kind="$1"
	local path="$2"
	local note="${3:-}"
	local differed nfields i gline rline name gbody rbody
	step=$((step + 1))

	printf '\n[%02d] %s   (%s)\n' "$step" "$path" "$kind"
	[ -n "$note" ] && printf '     %s\n' "$note"

	if [ "$kind" = "go-only" ]; then
		capture "$GO_BASE" "$path" go
		printf "  %-${LABEL_W}s %s\n" "status" "$(awk 'NR == 1 { print $2 }' "$WORK/go.h")"
		printf "  %-${LABEL_W}s %s\n" "body" "$(show_body "$WORK/go.b")"
		printf '  SKIPPED on RS: this route is not in the ported slice\n'
		n_skip=$((n_skip + 1))
		return 0
	fi

	capture "$GO_BASE" "$path" go
	capture "$RS_BASE" "$path" rs

	printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" "field" "GO" "RS"
	printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" "date" "<masked>" "<masked>"

	differed=0
	# `.f` holds one `name: value` per line in the same order on both sides, so
	# reading them in step is enough and no lookup is needed.
	nfields="$(awk 'END { print NR }' "$WORK/go.f")"
	i=0
	while [ "$i" -lt "$nfields" ]; do
		i=$((i + 1))
		gline="$(awk -v n="$i" 'NR == n' "$WORK/go.f")"
		rline="$(awk -v n="$i" 'NR == n' "$WORK/rs.f")"
		name="${gline%%: *}"
		row "$name" "${gline#*: }" "${rline#*: }" || differed=1
	done

	gbody="$(show_body "$WORK/go.n")"
	rbody="$(show_body "$WORK/rs.n")"
	if diff -q "$WORK/go.n" "$WORK/rs.n" >/dev/null 2>&1; then
		printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" "body" \
			"$(clip "$gbody" "$VALUE_W")" "$(clip "$rbody" "$VALUE_W")"
	else
		printf "  %-${LABEL_W}s %-${VALUE_W}s | %s\n" "body" \
			"$(clip "$gbody" "$VALUE_W")" "$(clip "$rbody" "$VALUE_W")"
		printf "    != GO: %s\n" "$gbody"
		printf "    != RS: %s\n" "$rbody"
		differed=1
	fi

	case "$kind:$differed" in
	compare:0)
		printf '  VERDICT: match\n'
		n_match=$((n_match + 1))
		;;
	compare:1)
		printf '  VERDICT: MISMATCH\n'
		n_diff=$((n_diff + 1))
		;;
	expected:0)
		printf '  VERDICT: match (the expected difference did not appear)\n'
		n_match=$((n_match + 1))
		;;
	expected:1)
		printf '  VERDICT: expected difference, not counted as a failure\n'
		n_expected=$((n_expected + 1))
		;;
	observe:*)
		printf '  VERDICT: observed, not compared (the value is live)\n'
		n_observe=$((n_observe + 1))
		;;
	esac
}

# The three net_probe runs, whose raw round trips are the number the trial is
# for. The bodies are compared with the digits normalised; the digits themselves
# are printed here.
probe_round() {
	request compare "/api-sdk/net_probe?serial=$SERIAL" \
		"the rttMs digits are normalised before the compare and printed raw below"
	printf '  raw rttMs   GO: %s   RS: %s\n' \
		"$(sed -n 's/.*"rttMs":\([^,}]*\).*/\1/p' "$WORK/go.b")" \
		"$(sed -n 's/.*"rttMs":\([^,}]*\).*/\1/p' "$WORK/rs.b")"
}

printf 'sdk-trial-diff\n'
printf '  GO_BASE %s\n' "$GO_BASE"
printf '  RS_BASE %s\n' "$RS_BASE"
printf '  SERIAL  %s\n' "$SERIAL"
if [ "$GO_BASE" = "$RS_BASE" ]; then
	printf '  both bases are the same server, so this run is testing the script\n'
fi

request go-only "/api/is_running" \
	"the Go server's own health probe; the ported slice does not serve it"

request compare "/api-sdk/conn_test?serial=$SERIAL" \
	"the connect preamble: this is the request that dials the robot"
request compare "/api-sdk/conn_test?serial=deadbeef" \
	"an unknown serial, which never dials and answers the doubled error prefix"

probe_round
probe_round
probe_round

request compare "/api-sdk/get_stim_status?serial=$SERIAL" \
	"before any event stream, so both sides answer the non-JSON sentinel"
request compare "/api-sdk/begin_event_stream?serial=$SERIAL" \
	"claims the stream on both servers at once, which the robot allows"

printf '\n>>> PET THE ROBOT ON ITS BACK FOR THE NEXT FIVE SECONDS <<<\n'
printf '    stim only rises when the robot is being touched or spoken to.\n'
printf '    A run of five zeroes means the readings arrived and nothing happened,\n'
printf '    which proves less than a non-zero one. Five sentinels mean the stream\n'
printf '    never opened.\n'
i=0
while [ "$i" -lt 5 ]; do
	i=$((i + 1))
	request observe "/api-sdk/get_stim_status?serial=$SERIAL" \
		"stim sample $i of 5, one second apart"
	sleep 1
done

request compare "/api-sdk/stop_event_stream?serial=$SERIAL" \
	"releases the claim on both servers"
request compare "/api-sdk/get_stim_status?serial=$SERIAL" \
	"after the stop, so both sides answer the sentinel again"

request compare "/api-sdk/begin_cam_stream?serial=$SERIAL" \
	"a no-op on both sides: Go's only statement in this arm is commented out"
request compare "/api-sdk/stop_cam_stream?serial=$SERIAL" \
	"nothing is streaming, and both sides answer done anyway"

request compare "/api-sdk/debug?serial=bogus" \
	"preamble-exempt, so a bogus serial still reaches the 404"
request compare "/api-sdk/does_not_exist?serial=$SERIAL" \
	"an unknown route under the prefix, which pays for the preamble first"
request compare "/api-sdk" \
	"the bare subtree prefix, which the mux answers with a 301"

request expected "/api/get_bot_status" \
	"deviation 2: nothing drives the Rust pinger yet, so RS reports disconnected
     with timesince -1 where Go reports a live status. Any other difference here
     is real."

request compare "/no-such-path" \
	"the root file server's 404, which carries no Cache-Control"

request compare "/api-sdk/disconnect?serial=$SERIAL" \
	"drops the cached connection after a three second settle on both sides"
printf '  time_total  GO: %ss   RS: %ss   (Go answers this route in about 3.00 s)\n' \
	"$(awk '{ printf "%s", $0 }' "$WORK/go.t")" "$(awk '{ printf "%s", $0 }' "$WORK/rs.t")"

request compare "/api-sdk/conn_test?serial=$SERIAL" \
	"straight after the disconnect, so both sides have to dial again"

# The one look at get_sdk_info: the first 40 bytes, for the key order, with
# everything after the opening of the GUID replaced.
step=$((step + 1))
printf '\n[%02d] %s   (observe)\n' "$step" "/api-sdk/get_sdk_info?serial=$SERIAL"
printf '     the first 40 bytes only, for the key order; the GUID is replaced\n'
for base_name in GO RS; do
	if [ "$base_name" = "GO" ]; then base="$GO_BASE"; else base="$RS_BASE"; fi
	printf "  %-${LABEL_W}s %s\n" "$base_name" \
		"$(curl -s -m "$TIMEOUT" "$base/api-sdk/get_sdk_info?serial=$SERIAL" |
			head -c 40 | sed 's/\("global_guid":"\).*/\1<guid>.../')"
done
n_observe=$((n_observe + 1))

printf '\nsummary\n'
printf '  match     %d\n' "$n_match"
printf '  mismatch  %d\n' "$n_diff"
printf '  expected  %d\n' "$n_expected"
printf '  observed  %d\n' "$n_observe"
printf '  skipped   %d\n' "$n_skip"

if [ "$n_diff" -gt 0 ]; then
	printf '\nFAIL: %d compared request(s) differed\n' "$n_diff"
	exit 1
fi
printf '\nPASS: every compared request matched\n'
exit 0
