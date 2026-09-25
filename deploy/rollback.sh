#!/bin/bash
# Roll jev-discord-bot back to the previously installed binary (and, with
# --config, the previous env file), then restart the service.
#
#   sudo deploy/rollback.sh [--config]
#
# Swaps the /opt/jev-discord-bot/jev-discord-bot and .../previous symlinks,
# so running it twice returns to where you started. --config swaps
# /etc/jev-discord-bot/env and env.previous the same way.
# Testing without root: JEV_DEPLOY_ROOT / JEV_DEPLOY_DRYRUN as in install.sh.
set -euo pipefail
set +x

ROOT=${JEV_DEPLOY_ROOT:-}
DRY=${JEV_DEPLOY_DRYRUN:-0}
OPT=$ROOT/opt/jev-discord-bot
ETC=$ROOT/etc/jev-discord-bot
UNIT=jev-discord-bot.service

say() { printf 'rollback: %s\n' "$*"; }
die() { printf 'rollback: ERROR: %s\n' "$*" >&2; exit 1; }
priv() { if [ "$DRY" = 1 ]; then say "[dry-run] $*"; else "$@"; fi; }

CONFIG=0
for a in "$@"; do
    case "$a" in
        --config) CONFIG=1 ;;
        *) die "usage: rollback.sh [--config]" ;;
    esac
done
[ "$DRY" = 1 ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"

CUR=$(readlink "$OPT/jev-discord-bot" 2>/dev/null) || die "no current binary symlink"
PREV=$(readlink "$OPT/previous" 2>/dev/null) || die "no previous binary to roll back to"
[ -x "$OPT/$PREV" ] || die "previous binary $OPT/$PREV missing"
ln -sfn "$PREV" "$OPT/jev-discord-bot"
ln -sfn "$CUR" "$OPT/previous"
say "binary: $CUR -> $PREV ($(sed -n 's/^commit=//p' "$OPT/$PREV.source" 2>/dev/null || echo '?'))"

if [ "$CONFIG" = 1 ]; then
    [ -f "$ETC/env.previous" ] || die "no $ETC/env.previous"
    mv -f "$ETC/env" "$ETC/env.swap"
    mv -f "$ETC/env.previous" "$ETC/env"
    mv -f "$ETC/env.swap" "$ETC/env.previous"
    say "config: env <-> env.previous swapped (keys now: $(cut -d= -f1 "$ETC/env" | tr '\n' ' '))"
fi

priv systemctl restart "$UNIT"
say "service restarted"
priv systemctl --no-pager --lines=0 status "$UNIT" || true
