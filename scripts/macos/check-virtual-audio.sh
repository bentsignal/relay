#!/usr/bin/env bash
# End-to-end check of Relay's macOS virtual devices. Needs RelayAudio.driver
# installed. Runs a throwaway engine (temporary RELAY_DATA_DIR) and verifies:
#   1. "Relay" is absent while no engine runs
#   2. "Relay" appears once the engine runs, and the engine reports Ready
#   3. audio played to "Relay" reaches the engine (Desktop strip meter)
#   4. audio the engine sends to the Relay Mic bus is recorded from "Relay"
#   5. killing the engine removes "Relay" and the default output falls back
# The original default output is restored at the end. Nothing is played on
# your speakers (the test tone is only routed to the Relay microphone).
set -uo pipefail
cd "$(dirname "$0")/../.."

fail=0
pass() { echo "PASS  $*"; }
bad() { echo "FAIL  $*"; fail=1; }

if [[ ! -f /Library/Audio/Plug-Ins/HAL/RelayAudio.driver/Contents/MacOS/RelayAudio ]]; then
    echo "RelayAudio.driver isn't installed in /Library/Audio/Plug-Ins/HAL" >&2
    exit 2
fi

cargo build -q -p relay-engine -p relay-ipc --example relayctl -p relay-audio --example probe --example default_device || exit 2
bin=target/debug
data=$(mktemp -d "${TMPDIR:-/tmp}/relay-check.XXXXXX")
export RELAY_DATA_DIR="$data"
engine_pid=""
cleanup() {
    [[ -n $engine_pid ]] && kill "$engine_pid" 2>/dev/null
    [[ -n ${orig_out:-} ]] && "$bin/examples/default_device" output "$orig_out" 2>/dev/null
    rm -rf "$data"
}
trap cleanup EXIT

relay_listed() { system_profiler SPAudioDataType 2>/dev/null | grep -qE '^        Relay:$'; }
default_out() { "$bin/examples/default_device" | sed -n 's/^output //p'; }
ctl() { "$bin/examples/relayctl" "$@" 2>&1; }

orig_out=$(default_out)
echo "default output: $orig_out"

pkill -x relay-engine 2>/dev/null && sleep 2
if relay_listed; then bad "Relay is listed before the engine runs"; else pass "Relay absent without the engine"; fi

RUST_LOG=info "$bin/relay-engine" >"$data/engine.out" 2>&1 &
engine_pid=$!
sleep 4
if relay_listed; then pass "Relay listed while the engine runs"; else bad "Relay not listed while the engine runs"; fi

state=$(ctl 2)
echo "$state" | head -8
if echo "$state" | grep -q "virtual=Ready"; then pass "engine reports Ready"; else bad "engine not Ready"; fi
desktop=$(echo "$state" | sed -n 's/^  strip #\([0-9]*\) Desktop .*/\1/p' | head -1)
mic_strip=$(echo "$state" | sed -n 's/^  strip #\([0-9]*\) Mic .*/\1/p' | head -1)
mic_bus=$(echo "$state" | sed -n 's/^  bus #\([0-9]*\) .*VirtualMic.*/\1/p' | head -1)
speakers=$(echo "$state" | sed -n 's/^  bus #\([0-9]*\) .*Device {.*/\1/p' | head -1)
echo "desktop strip $desktop, mic strip $mic_strip, mic bus $mic_bus, speakers bus $speakers"

# Keep the tone off the speakers and the real mic out of the Relay mic, then
# loop Desktop into the Relay mic.
cmds=("{\"cmd\":\"set_route\",\"strip\":$desktop,\"bus\":$mic_bus,\"on\":true}")
[[ -n $speakers ]] && cmds+=("{\"cmd\":\"set_route\",\"strip\":$desktop,\"bus\":$speakers,\"on\":false}")
[[ -n $mic_strip ]] && cmds+=("{\"cmd\":\"set_strip_mute\",\"id\":$mic_strip,\"mute\":true}")
ctl 1 "${cmds[@]}" >/dev/null

"$bin/examples/probe" Relay 6 >"$data/probe.out" 2>&1 &
probe_pid=$!
sleep 1
meters=$(ctl 4)
wait $probe_pid
cat "$data/probe.out"
peak=$(echo "$meters" | grep -o "strips\[[^]]*" | grep -o "$desktop:[0-9.]*" | cut -d: -f2 | sort -n | tail -1)
echo "Desktop strip peak: ${peak:-none}"
if awk -v p="${peak:-0}" 'BEGIN{exit !(p > 0.3)}'; then pass "audio played to Relay reaches the engine"; else bad "no signal from Relay output in the engine"; fi
mic_peak=$(grep -o 'mic peak [0-9.]*' "$data/probe.out" | awk '{print $3}' | sort -n | tail -1)
mic_hz=$(grep -o '~[0-9]* Hz' "$data/probe.out" | tail -2 | head -1 | tr -dc 0-9)
echo "Relay input: peak ${mic_peak:-none}, ~${mic_hz:-?} Hz"
if awk -v p="${mic_peak:-0}" -v h="${mic_hz:-0}" 'BEGIN{exit !(p > 0.3 && h > 900 && h < 1100)}'; then
    pass "engine output is recorded from the Relay input"
else
    bad "Relay input didn't carry the engine's tone"
fi

ctl 1 '{"cmd":"make_relay_default","on":true}' >/dev/null
sleep 3
if [[ $(default_out) == relay.device.main ]]; then pass "Relay became the default output"; else bad "Relay isn't the default output ($(default_out))"; fi

kill -9 "$engine_pid"
wait "$engine_pid" 2>/dev/null
engine_pid=""
sleep 3
if relay_listed; then bad "Relay still listed after the engine died"; else pass "Relay removed after the engine died"; fi
after=$(default_out)
if [[ -n $after && $after != relay.device.main ]]; then pass "default output fell back to $after"; else bad "default output is '$after'"; fi

if [[ $fail == 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED; engine log:"
    tail -30 "$data/engine.out"
fi
exit $fail
