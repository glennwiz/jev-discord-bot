#!/bin/bash
# Unprivileged dry-run test of deploy/install.sh and deploy/rollback.sh in a
# scratch root on disk (not /tmp). Dummy secrets only; checks they are never
# printed. Needs a release build at target/release/jev-discord-bot.
#
#   deploy/test-dryrun.sh        -> prints PASS/FAIL per check, then DEPLOY-TEST PASS|FAIL
set -u
REPO=${REPO:-$(cd "$(dirname "$0")/.." && pwd)}
T=${JEV_TEST_SCRATCH:-$HOME/jev-scratch}/deploy-test
rm -rf "$T" && mkdir -p "$T/root" "$T/bin"
export JEV_DEPLOY_ROOT=$T/root JEV_DEPLOY_DRYRUN=1
# Default: an empty fake process table, so steps do not depend on whatever
# bots happen to run on this machine. Checks that want real /proc say so.
mkdir -p "$T/noproc"
export JEV_DEPLOY_PROC=$T/noproc
fail=0
check() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }

cat >"$T/src.env" <<'E'
# comment line
DISCORD_TOKEN=DUMMY_TOKEN_SECRET_111
export DISCORD_GUILD_ID=123456789012345678
JEVMODEL_API_KEY="DUMMY_KEY_SECRET_222"
JEV_BASE_URL=https://api.typesafe.ai
DISCORD_API_PROXY=http://127.0.0.1:9
UNRELATED=x
E
cp "$REPO/target/release/jev-discord-bot" "$T/bin/v1" && chmod +x "$T/bin/v1"
cp /usr/bin/true "$T/bin/v2"
OUT=$T/out.txt
# Every step's output also goes to $T/all.txt: the secret check scans ALL of
# it, not just the last step (OUT is cleared between steps).
run() { "$@" >>"$OUT" 2>&1; echo "exit=$?" >>"$OUT"; cat "$OUT" >>"$T/all.txt"; }

run bash "$REPO/deploy/install.sh" "$T/bin/v1" 1111111aaaaa "$T/src.env"
ENVF=$T/root/etc/jev-discord-bot/env
check "env keys renamed+allow-listed" '[ "$(cut -d= -f1 "$ENVF" | tr "\n" " ")" = "DISCORD_TOKEN DISCORD_GUILD_ID TYPESAFE_API_KEY JEV_BASE_URL " ]'
check "env has no proxy seam" '! grep -q DISCORD_API_PROXY "$ENVF"'
check "env mode 0640" '[ "$(stat -c %a "$ENVF")" = 640 ]'
check "etc dir mode 0750" '[ "$(stat -c %a "$T/root/etc/jev-discord-bot")" = 750 ]'
check "current -> v1" '[ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = jev-discord-bot-1111111aaaaa ]'
check "unit installed" 'cmp -s "$REPO/deploy/jev-discord-bot.service" "$T/root/etc/systemd/system/jev-discord-bot.service"'
# useradd only runs when jevbot does not exist yet on this host.
if id jevbot >/dev/null 2>&1; then
    check "existing jevbot user reused, chown+enable dry-run" 'grep -q "user jevbot exists" "$OUT" && ! grep -q "\[dry-run\] useradd" "$OUT" && grep -q "\[dry-run\] chown root:jevbot" "$OUT" && grep -q "\[dry-run\] systemctl enable" "$OUT"'
else
    check "dry-run useradd/chown/enable" 'grep -q "\[dry-run\] useradd" "$OUT" && grep -q "\[dry-run\] chown root:jevbot" "$OUT" && grep -q "\[dry-run\] systemctl enable" "$OUT"'
fi

: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v1" 1111111aaaaa "$T/src.env"
check "rerun idempotent" 'grep -q "already installed" "$OUT" && grep -q "current already" "$OUT" && grep -q "left unchanged" "$OUT" && ! [ -e "$T/root/opt/jev-discord-bot/previous" ]'

: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v2" 1111111aaaaa
check "refuses to overwrite a released version" 'grep -q "refusing to overwrite" "$OUT" && grep -q "exit=1" "$OUT"'

: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v2" 2222222bbbbb
check "v2 current, v1 previous" '[ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = jev-discord-bot-2222222bbbbb ] && [ "$(readlink "$T/root/opt/jev-discord-bot/previous")" = jev-discord-bot-1111111aaaaa ]'

: >"$OUT"; run bash "$REPO/deploy/rollback.sh"
check "rollback -> v1" '[ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = jev-discord-bot-1111111aaaaa ] && [ "$(readlink "$T/root/opt/jev-discord-bot/previous")" = jev-discord-bot-2222222bbbbb ] && grep -q "\[dry-run\] systemctl restart" "$OUT"'
: >"$OUT"; run bash "$REPO/deploy/rollback.sh"
check "rollback twice -> v2" '[ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = jev-discord-bot-2222222bbbbb ]'

: >"$OUT"; run env JEV_DEPLOY_PROC=/proc bash "$REPO/deploy/install.sh" "$T/bin/v2" 2222222bbbbb --start
if pgrep -f 'target/release/jev-discord-bot' >/dev/null; then
    check "--start refused while ad-hoc bot runs" 'grep -q "ad-hoc jev-discord-bot is still running" "$OUT" && ! grep -q "\[dry-run\] systemctl restart" "$OUT"'
else
    echo "SKIP --start refusal (no ad-hoc bot running)"
fi

# --start guard against a fake process table: install.sh's own argv holds a
# target/release path, which must NOT count as an ad-hoc bot.
mkdir -p "$T/rel/target/release" "$T/fakeproc/1" "$T/fakeproc/2"
cp "$T/bin/v2" "$T/rel/target/release/jev-discord-bot"
ln -s /usr/bin/bash "$T/fakeproc/1/exe"
ln -s "$T/root/opt/jev-discord-bot/jev-discord-bot-2222222bbbbb" "$T/fakeproc/2/exe"
: >"$OUT"; run env JEV_DEPLOY_PROC="$T/fakeproc" bash "$REPO/deploy/install.sh" "$T/rel/target/release/jev-discord-bot" 2222222bbbbb --start
check "--start proceeds: own target/release argv and the service's binary do not count" 'grep -q "\[dry-run\] systemctl restart" "$OUT" && ! grep -q "still running" "$OUT"'
mkdir -p "$T/fakeproc/3"; ln -s /home/someone/dev/jev-discord-bot/target/release/jev-discord-bot "$T/fakeproc/3/exe"
: >"$OUT"; run env JEV_DEPLOY_PROC="$T/fakeproc" bash "$REPO/deploy/install.sh" "$T/rel/target/release/jev-discord-bot" 2222222bbbbb --start
check "--start refused for an ad-hoc bot exe" 'grep -q "still running (pid exe: 3 /home/someone" "$OUT" && ! grep -q "\[dry-run\] systemctl restart" "$OUT"'
rm -f "$T/fakeproc/3/exe"; ln -s "/home/someone/target/release/jev-discord-bot (deleted)" "$T/fakeproc/3/exe"
: >"$OUT"; run env JEV_DEPLOY_PROC="$T/fakeproc" bash "$REPO/deploy/install.sh" "$T/rel/target/release/jev-discord-bot" 2222222bbbbb --start
check "--start refused for a replaced (deleted) ad-hoc exe" 'grep -q "still running" "$OUT"'

# rollback.sh: refused (and nothing changed) while an ad-hoc bot runs.
cur_before=$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot"); prev_before=$(readlink "$T/root/opt/jev-discord-bot/previous")
: >"$OUT"; run env JEV_DEPLOY_PROC="$T/fakeproc" bash "$REPO/deploy/rollback.sh"
check "rollback refused while an ad-hoc bot runs, symlinks untouched" 'grep -q "still running (pid exe: 3 " "$OUT" && ! grep -q "\[dry-run\] systemctl restart" "$OUT" && [ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = "$cur_before" ] && [ "$(readlink "$T/root/opt/jev-discord-bot/previous")" = "$prev_before" ]'
# rollback.sh --config with no env.previous: refused before the binary swap.
: >"$OUT"; run bash "$REPO/deploy/rollback.sh" --config
check "rollback --config without env.previous refused, binary untouched" 'grep -q "no .*env.previous" "$OUT" && [ "$(readlink "$T/root/opt/jev-discord-bot/jev-discord-bot")" = "$cur_before" ]'

sed -i 's/^JEV_BASE_URL=.*/JEV_TIMEOUT_SECS=15/' "$T/src.env"
: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v2" 2222222bbbbb "$T/src.env" --refresh-env
check "refresh-env keeps env.previous" '[ -f "$ENVF.previous" ] && grep -q "^JEV_TIMEOUT_SECS=" "$ENVF" && grep -q "^JEV_BASE_URL=" "$ENVF.previous" && [ "$(stat -c %a "$ENVF.previous")" = 640 ]'
: >"$OUT"; run bash "$REPO/deploy/rollback.sh" --config
check "rollback --config swaps env" 'grep -q "^JEV_BASE_URL=" "$ENVF" && grep -q "^JEV_TIMEOUT_SECS=" "$ENVF.previous"'

: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v1" 3333333ccccc "$T/src.env" --bogus
check "unknown option refused" 'grep -q "unknown option" "$OUT"'
mv "$ENVF" "$ENVF.keep"; printf 'DISCORD_TOKEN=x\n' >"$T/partial.env"
: >"$OUT"; run bash "$REPO/deploy/install.sh" "$T/bin/v1" 1111111aaaaa "$T/partial.env"
check "missing keys named, not written" 'grep -q "lacks: DISCORD_GUILD_ID TYPESAFE_API_KEY" "$OUT" && ! [ -f "$ENVF" ] && ! ls "$T/root/etc/jev-discord-bot"/.env.* >/dev/null 2>&1'
mv "$ENVF.keep" "$ENVF"

ALL=$(cat "$T/all.txt"; bash "$REPO/deploy/install.sh" "$T/bin/v2" 2222222bbbbb 2>&1)
check "no secret value ever printed" '! grep -qE "DUMMY_(TOKEN|KEY)_SECRET" <<<"$ALL"'
rm -rf "$T"
echo "DEPLOY-TEST $([ $fail = 0 ] && echo PASS || echo FAIL)"
