# Shared by deploy/install.sh and deploy/rollback.sh (sourced, not run).
# Callers set OPT (the install dir, possibly under JEV_DEPLOY_ROOT) and
# PROC (/proc, or JEV_DEPLOY_PROC for tests).

# Print "<pid> <exe>" for every running jev-discord-bot that is NOT the
# service's own binary - e.g. an ad-hoc bot in tmux with the same Discord
# token, which must never run alongside the service (double answers).
# Matches on each process's executable, not its command line: install.sh's
# own argv (and sudo's) contains a binary path, but their exe is bash/sudo.
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

# die() unless no ad-hoc bot runs; call before any (re)start of the service.
require_no_adhoc_bot() {
    local running
    running=$(adhoc_bots)
    if [ -n "$running" ]; then
        die "an ad-hoc jev-discord-bot is still running (pid exe: $running); stop it first - same Discord token"
    fi
}
