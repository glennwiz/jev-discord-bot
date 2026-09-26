#!/bin/bash
# Verify the deployed jev-discord-bot service and record evidence (#5).
# Run as root in tmux jev:deploy; output is safe to keep (no secret values):
#
#   sudo deploy/verify-service.sh <pinned-release-dir> <previous-release-dir> [--error-path] \
#       2>&1 | tee ~/jev-logs/verify-service.log
#
#   <pinned-release-dir>    e.g. ~/jev-release/d22cb9c (its deploy/ scripts are used)
#   <previous-release-dir>  e.g. ~/jev-release/7a26075 (installed, then rolled back from)
#   --error-path            also show a real TypeSafe error reply (interactive:
#                           asks you to run one /jev command in Discord)
#   --readonly              only print versions, state and command evidence
#                           (works without root; used to test this script)
#
# Steps: 1 versions/state  2 kill -9 -> restart  3 systemctl stop -> graceful
# 4 start  5 rollback exercise  6 TypeSafe error path  7 Discord evidence
# 8 secret scan. Ends with the service running the pinned release.
#
# Pacing: the unit allows 5 starts per 300 s (StartLimitBurst), so every
# deliberate (re)start is preceded by `systemctl reset-failed`; and Discord
# rate-limits gateway logins and command registration, so after every
# restart we wait for "registered" before the next one (REG_WAIT).
set -uo pipefail
set +x

UNIT=jev-discord-bot.service
OPT=/opt/jev-discord-bot
ENVF=/etc/jev-discord-bot/env
DROPIN_DIR=/run/systemd/system/$UNIT.d
DROPIN=$DROPIN_DIR/zz-verify-error-path.conf
ERR_ENV=/run/jev-verify-error-path.env
REG_WAIT=90

READONLY=0 ERROR_PATH=0 ARGS=()
for a in "$@"; do
    case "$a" in
        --readonly) READONLY=1 ;;
        --error-path) ERROR_PATH=1 ;;
        -*) echo "verify: unknown option $a" >&2; exit 2 ;;
        *) ARGS+=("$a") ;;
    esac
done

step() { printf '\n=== %s\n' "$*"; }
ok() { printf 'PASS %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; FAILED=1; }
FAILED=0
prop() { systemctl show "$UNIT" -p "$1" --value; }
now_ms() { date +%s%3N; }
# Journal lines of this unit at or after an epoch in ms (millisecond
# precise: --since only takes whole seconds, so filter the rest in awk).
journal_since() {
    journalctl -u "$UNIT" --since "@$(( $1 / 1000 ))" -o short-unix --no-pager 2>/dev/null |
        awk -v t="$1" '{ split($1, a, "."); if (a[1] * 1000 + substr(a[2] "000", 1, 3) >= t) print }'
}
# Wait (<= $2 s) until the unit is active with a MainPID other than $1.
wait_new_pid() {
    local old=$1 limit=$2 t0 pid
    t0=$(now_ms)
    while [ $(( $(now_ms) - t0 )) -lt $(( limit * 1000 )) ]; do
        pid=$(prop MainPID)
        if [ "$(prop ActiveState)" = active ] && [ "$pid" != 0 ] && [ "$pid" != "$old" ]; then
            echo "$pid $(( $(now_ms) - t0 ))"
            return 0
        fi
        sleep 0.2
    done
    return 1
}
# Wait (<= $2 s) for "registered N guild command(s)" in the journal since $1 ms.
wait_registered() {
    local since=$1 limit=$2 t0
    t0=$(now_ms)
    while [ $(( $(now_ms) - t0 )) -lt $(( limit * 1000 )) ]; do
        journal_since "$since" | grep -q 'registered [0-9]* guild command' && return 0
        sleep 0.5
    done
    return 1
}
# Clear the start-rate counter before a deliberate (re)start (see Pacing).
reset() { systemctl reset-failed "$UNIT" 2>/dev/null || true; }
running_version() { local p; p=$(prop MainPID); readlink "/proc/$p/exe" 2>/dev/null | sed 's|.*/||'; }

step "1. versions and state ($(date -u +%FT%TZ))"
echo "host: $(uname -srm); $(systemctl --version | head -1)"
echo "current -> $(readlink $OPT/jev-discord-bot); previous -> $(readlink $OPT/previous 2>/dev/null || echo none)"
for s in $OPT/*.source; do echo "$(basename "$s" .source): $(tr '\n' ' ' <"$s")"; done
echo "sha256 current: $(sha256sum "$OPT/$(readlink $OPT/jev-discord-bot)" | cut -d' ' -f1)"
echo "unit: enabled=$(systemctl is-enabled $UNIT) active=$(prop ActiveState)/$(prop SubState) MainPID=$(prop MainPID) NRestarts=$(prop NRestarts) User=$(prop User)"
pid=$(prop MainPID)
echo "process: user=$(ps -o user= -p "$pid" 2>/dev/null) exe=$(running_version)"
if [ "$(id -u)" = 0 ]; then
    echo "env file: $(stat -c '%a %U:%G' $ENVF) keys: $(cut -d= -f1 $ENVF | tr '\n' ' ')"
    [ "$(stat -c '%a %U:%G' $ENVF)" = "640 root:jevbot" ] && ok "env file 0640 root:jevbot" || bad "env file mode/owner"
fi
[ "$(ps -o user= -p "$pid" 2>/dev/null | tr -d ' ')" = jevbot ] && ok "runs as jevbot" || bad "not running as jevbot"
[ "$(systemctl is-enabled $UNIT)" = enabled ] && ok "unit enabled (starts at boot)" || bad "unit not enabled"

if [ "$READONLY" = 0 ]; then
    [ "$(id -u)" = 0 ] || { echo "verify: run as root (or use --readonly)" >&2; exit 1; }
    [ ${#ARGS[@]} -ge 2 ] || { echo "verify: need <pinned-release-dir> <previous-release-dir>" >&2; exit 2; }
    PIN=${ARGS[0]} PREV=${ARGS[1]}
    PIN_C=$(basename "$PIN") PREV_C=$(basename "$PREV")
    [ -x "$PIN/deploy/install.sh" ] && [ -x "$PREV/target/release/jev-discord-bot" ] || { echo "verify: release dirs incomplete" >&2; exit 2; }

    step "2. kill -9 MainPID -> systemd restarts it (Restart=on-failure, RestartSec=5)"
    reset
    old=$(prop MainPID) n0=$(prop NRestarts) t=$(now_ms)
    kill -KILL "$old"
    if res=$(wait_new_pid "$old" 30); then
        echo "killed $old; new MainPID ${res% *} after ${res#* } ms; NRestarts $n0 -> $(prop NRestarts)"
        [ "$(prop NRestarts)" = $(( n0 + 1 )) ] && ok "restarted after kill -9, NRestarts+1" || bad "NRestarts did not increase by 1"
        wait_registered "$t" $REG_WAIT && ok "reconnected (registered guild command)" || bad "no re-registration after restart"
    else
        bad "no restart within 30 s of kill -9"
    fi
    journal_since "$t" | grep -E 'Main process exited|Scheduled restart|Started|connected as|registered'

    step "3. systemctl stop -> graceful shutdown"
    t=$(now_ms)
    systemctl stop "$UNIT"
    took=$(( $(now_ms) - t ))
    echo "stop took ${took} ms; Result=$(prop Result) ExecMainCode=$(prop ExecMainCode) ExecMainStatus=$(prop ExecMainStatus)"
    journal_since "$t" | grep -E 'shutdown:|Stopping|Stopped|Deactivated'
    journal_since "$t" | grep -q 'shutdown: SIGTERM received' && ok "SIGTERM handled" || bad "no 'shutdown: SIGTERM received'"
    journal_since "$t" | grep -q 'shutdown: clean' && ok "'shutdown: clean' logged" || bad "no 'shutdown: clean'"
    [ "$took" -lt 16000 ] && ok "stopped within DRAIN_TIMEOUT (15 s)" || bad "stop took ${took} ms"
    [ "$(prop ExecMainStatus)" = 0 ] && ok "exit status 0" || bad "exit status $(prop ExecMainStatus)"

    step "4. systemctl start"
    reset
    t=$(now_ms)
    systemctl start "$UNIT"
    wait_registered "$t" $REG_WAIT && ok "started and registered" || bad "no registration after start"

    step "5. rollback exercise: install $PREV_C as current, then deploy/rollback.sh back to $PIN_C"
    echo "note: $PREV_C and $PIN_C differ only in deploy/tests/README, so their binaries are byte-identical;"
    echo "      this proves the switch+restart mechanism, not a behaviour change."
    reset
    t=$(now_ms)
    "$PIN/deploy/install.sh" "$PREV/target/release/jev-discord-bot" "$PREV_C" --start
    wait_registered "$t" $REG_WAIT && ok "registered after install --start" || bad "no registration after install --start"
    [ "$(running_version)" = "jev-discord-bot-$PREV_C" ] && ok "running $PREV_C after install --start" || bad "running $(running_version), expected $PREV_C"
    reset
    t=$(now_ms)
    "$PIN/deploy/rollback.sh"
    wait_registered "$t" $REG_WAIT && ok "registered after rollback" || bad "no registration after rollback"
    [ "$(running_version)" = "jev-discord-bot-$PIN_C" ] && ok "rolled back: running $PIN_C" || bad "running $(running_version), expected $PIN_C"
    [ "$(readlink $OPT/previous)" = "jev-discord-bot-$PREV_C" ] && ok "previous -> $PREV_C kept for a real rollback" || bad "previous link"

    if [ "$ERROR_PATH" = 1 ]; then
        step "6. TypeSafe error path: temporary base URL with a non-existent path (real TypeSafe 404, not billed)"
        mkdir -p "$DROPIN_DIR"
        printf 'JEV_BASE_URL=https://api.typesafe.ai/jev-verify-error-path\n' >"$ERR_ENV"
        chmod 0644 "$ERR_ENV"
        printf '[Service]\n# Temporary, from deploy/verify-service.sh; removed at the end of step 6.\nEnvironmentFile=%s\n' "$ERR_ENV" >"$DROPIN"
        systemctl daemon-reload
        reset
        t=$(now_ms)
        systemctl restart "$UNIT"
        wait_registered "$t" $REG_WAIT || bad "no registration with the error-path config"
        journal_since "$t" | grep -o 'jev_base_url: "[^"]*"'
        echo
        echo ">>> Now run ONE command in the test guild, e.g.:"
        echo ">>>   /jev noul text:error path check question:Is this a test?"
        echo ">>> then press Enter here."
        read -r _ </dev/tty
        journal_since "$t" | grep -E '(choice|score|noul) (failed|ok|timing)'
        journal_since "$t" | grep -qE '(choice|score|noul) failed: .*HTTP 404' && ok "TypeSafe error reply path (HTTP 404) exercised" || bad "no '... failed: ... HTTP 404' line"
        rm -f "$DROPIN" "$ERR_ENV"
        rmdir "$DROPIN_DIR" 2>/dev/null || true
        systemctl daemon-reload
        reset
        t=$(now_ms)
        systemctl restart "$UNIT"
        wait_registered "$t" $REG_WAIT || bad "no registration after removing the error path"
        journal_since "$t" | grep -q 'jev_base_url: "https://api.typesafe.ai"' && ok "normal config restored" || bad "base URL not restored"
    fi
fi

step "7. Discord command evidence from the journal (this boot)"
journalctl -u "$UNIT" -b -o short-iso --no-pager 2>/dev/null | grep -E '(choice|score|noul) (ok|failed|timing)|discord reply failed' || echo "(none yet)"

if [ "$(id -u)" = 0 ]; then
    step "8. secret scan: env values never appear in the journal (values not printed)"
    while IFS='=' read -r k v; do
        # Only the real secrets; the guild id is logged on purpose.
        case "$k" in DISCORD_TOKEN | TYPESAFE_API_KEY) ;; *) continue ;; esac
        v=${v%\"}; v=${v#\"}; v=${v%\'}; v=${v#\'}
        [ ${#v} -ge 12 ] || { bad "$k value too short to scan"; continue; }
        # Pattern via a file descriptor, never argv (argv shows in /proc/*/cmdline).
        n=$(journalctl -u "$UNIT" -o cat --no-pager 2>/dev/null | grep -cF -f <(printf '%s\n' "$v"))
        echo "$k: occurrences=$n"
        [ "$n" = 0 ] || bad "$k value found in journal"
    done <"$ENVF"
fi

step "final: current -> $(readlink $OPT/jev-discord-bot), running $(running_version), $(prop ActiveState)/$(prop SubState), NRestarts=$(prop NRestarts)"
[ "$FAILED" = 0 ] && echo "VERIFY PASS" || echo "VERIFY FAIL"
