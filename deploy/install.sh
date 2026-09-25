#!/bin/bash
# Install or update jev-discord-bot as a hardened systemd service.
# Idempotent: re-running with the same binary changes nothing.
#
#   sudo deploy/install.sh <binary> <source-commit> [<source .env>] [--start]
#
#   <binary>        release build, e.g. target/release/jev-discord-bot
#   <source-commit> the git commit it was built from (recorded next to it)
#   <source .env>   only read when /etc/jev-discord-bot/env does not exist
#                   yet (or with --refresh-env); keys are copied by NAME,
#                   values are never printed
#   --start         (re)start the service afterwards; refused while an ad-hoc
#                   bot (same Discord token) is still running
#   --refresh-env   rebuild the env file from <source .env>, keeping the old
#                   one as env.previous
#
# Layout:
#   /opt/jev-discord-bot/jev-discord-bot-<commit>   versioned binaries (0755 root)
#   /opt/jev-discord-bot/jev-discord-bot            symlink -> current version (ExecStart)
#   /opt/jev-discord-bot/previous                   symlink -> version before it (rollback.sh)
#   /etc/jev-discord-bot/env                        0640 root:jevbot
#   /etc/systemd/system/jev-discord-bot.service
#
# Testing without root: JEV_DEPLOY_ROOT=<dir> prefixes every path,
# JEV_DEPLOY_DRYRUN=1 prints (instead of running) useradd/chown/systemctl,
# and JEV_DEPLOY_PROC=<dir> replaces /proc for the ad-hoc-bot check.
set -euo pipefail
set +x # never trace: the env file holds secrets

ROOT=${JEV_DEPLOY_ROOT:-}
DRY=${JEV_DEPLOY_DRYRUN:-0}
PROC=${JEV_DEPLOY_PROC:-/proc}
OPT=$ROOT/opt/jev-discord-bot
ETC=$ROOT/etc/jev-discord-bot
UNIT_DIR=$ROOT/etc/systemd/system
UNIT=jev-discord-bot.service
HERE=$(cd "$(dirname "$0")" && pwd)

say() { printf 'install: %s\n' "$*"; }
die() { printf 'install: ERROR: %s\n' "$*" >&2; exit 1; }
priv() { if [ "$DRY" = 1 ]; then say "[dry-run] $*"; else "$@"; fi; }

START=0 REFRESH_ENV=0 ARGS=()
for a in "$@"; do
    case "$a" in
        --start) START=1 ;;
        --refresh-env) REFRESH_ENV=1 ;;
        -*) die "unknown option $a" ;;
        *) ARGS+=("$a") ;;
    esac
done
[ ${#ARGS[@]} -ge 2 ] || die "usage: install.sh <binary> <source-commit> [<source .env>] [--start] [--refresh-env]"
BIN=${ARGS[0]} COMMIT=${ARGS[1]} ENV_SRC=${ARGS[2]:-}
[ "$DRY" = 1 ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"
[ -x "$BIN" ] || die "binary $BIN is missing or not executable"
[[ "$COMMIT" =~ ^[0-9a-f]{7,40}$ ]] || die "source commit must be a git hash, got '$COMMIT'"
[ -f "$HERE/$UNIT" ] || die "unit file $HERE/$UNIT missing"

# 1. Dedicated login-less user and group.
if id jevbot >/dev/null 2>&1; then
    say "user jevbot exists"
else
    priv useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/bin/nologin jevbot
    say "created system user jevbot"
fi

# 2. Directories.
mkdir -p "$OPT" "$ETC" "$UNIT_DIR"
chmod 0755 "$OPT"
chmod 0750 "$ETC"
priv chown root:jevbot "$ETC"

# 3. Environment file: allow-listed keys copied by name; values never shown.
KEYS="DISCORD_TOKEN DISCORD_GUILD_ID TYPESAFE_API_KEY JEV_BASE_URL JEV_TIMEOUT_SECS"
write_env() {
    local src=$1 tmp
    [ -f "$src" ] || die "source env $src not found"
    tmp=$(umask 077 && mktemp "$ETC/.env.XXXXXX")
    # KEY=value lines (optional 'export '); JEVMODEL_API_KEY is the old name
    # of TYPESAFE_API_KEY. DISCORD_API_PROXY (test seam) is never copied.
    awk -v keys="$KEYS" '
        BEGIN { n = split(keys, k, " "); for (i = 1; i <= n; i++) allow[k[i]] = 1 }
        /^[ \t]*(export[ \t]+)?[A-Z_][A-Z0-9_]*=/ {
            line = $0; sub(/^[ \t]*(export[ \t]+)?/, "", line)
            key = substr(line, 1, index(line, "=") - 1); val = substr(line, index(line, "=") + 1)
            if (key == "JEVMODEL_API_KEY") key = "TYPESAFE_API_KEY"
            if ((key in allow) && !(key in seen)) { seen[key] = 1; print key "=" val }
        }' "$src" >"$tmp"
    local missing=""
    for k in DISCORD_TOKEN DISCORD_GUILD_ID TYPESAFE_API_KEY; do
        grep -q "^$k=." "$tmp" || missing="$missing $k"
    done
    if [ -n "$missing" ]; then rm -f "$tmp"; die "source env lacks:$missing"; fi
    chmod 0640 "$tmp"
    priv chown root:jevbot "$tmp"
    if [ -f "$ETC/env" ]; then cp -p "$ETC/env" "$ETC/env.previous"; fi
    mv -f "$tmp" "$ETC/env"
    say "wrote $ETC/env with keys: $(cut -d= -f1 "$ETC/env" | tr '\n' ' ')"
}
if [ ! -f "$ETC/env" ]; then
    [ -n "$ENV_SRC" ] || die "no $ETC/env yet: pass the source .env"
    write_env "$ENV_SRC"
elif [ "$REFRESH_ENV" = 1 ]; then
    [ -n "$ENV_SRC" ] || die "--refresh-env needs the source .env"
    write_env "$ENV_SRC"
else
    say "env file exists, left unchanged (keys: $(cut -d= -f1 "$ETC/env" | tr '\n' ' '))"
fi
chmod 0640 "$ETC/env"
priv chown root:jevbot "$ETC/env"

# 4. Versioned binary, then current/previous symlinks.
VER=jev-discord-bot-${COMMIT:0:12}
if [ -f "$OPT/$VER" ] && cmp -s "$BIN" "$OPT/$VER"; then
    say "binary $VER already installed"
else
    [ ! -e "$OPT/$VER" ] || die "$OPT/$VER exists with different content; refusing to overwrite a released version"
    install -m 0755 "$BIN" "$OPT/$VER"
    priv chown root:root "$OPT/$VER"
    printf 'commit=%s\nsha256=%s\n' "$COMMIT" "$(sha256sum "$OPT/$VER" | cut -d' ' -f1)" >"$OPT/$VER.source"
    say "installed $VER"
fi
CUR=$(readlink "$OPT/jev-discord-bot" 2>/dev/null || true)
if [ "$CUR" != "$VER" ]; then
    if [ -n "$CUR" ]; then ln -sfn "$CUR" "$OPT/previous"; say "previous -> $CUR"; fi
    ln -sfn "$VER" "$OPT/jev-discord-bot"
    say "current -> $VER"
else
    say "current already -> $VER"
fi
install -m 0644 "$HERE/../README.md" "$OPT/README.md"

# 5. Unit.
if ! cmp -s "$HERE/$UNIT" "$UNIT_DIR/$UNIT"; then
    install -m 0644 "$HERE/$UNIT" "$UNIT_DIR/$UNIT"
    say "installed unit"
fi
priv systemctl daemon-reload
priv systemctl enable "$UNIT"

# 6. Optional (re)start - never alongside an ad-hoc bot with the same token.
# Matches on each process's executable, not its command line: this script's
# own argv (and sudo's) contains the binary path, but their exe is bash/sudo.
# The service's own binaries under /opt/jev-discord-bot do not count.
adhoc_bots() {
    local p exe
    for p in "$PROC"/[0-9]*; do
        exe=$(readlink "$p/exe" 2>/dev/null) || continue
        exe=${exe% (deleted)}
        case "$exe" in
            "$OPT"/* | /opt/jev-discord-bot/*) ;;
            */jev-discord-bot | */jev-discord-bot-*) printf '%s %s\n' "${p##*/}" "$exe" ;;
        esac
    done
}
if [ "$START" = 1 ]; then
    running=$(adhoc_bots)
    if [ -n "$running" ]; then
        die "an ad-hoc jev-discord-bot is still running (pid exe: $running); stop it first - same Discord token"
    fi
    priv systemctl restart "$UNIT"
    say "service restarted"
    priv systemctl --no-pager --lines=0 status "$UNIT" || true
fi
say "done: $(sed -n 's/^sha256=//p' "$OPT/$VER.source") $VER"
