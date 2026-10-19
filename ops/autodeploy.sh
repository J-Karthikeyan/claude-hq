#!/usr/bin/env bash
#
# Pull-based continuous deployment for a self-hosted Arena.
#
# Checks the tracked branch for new commits and, if there are any, hands the
# release to ops/release.sh. Install with ops/install-autodeploy.sh. Logs to
# /var/log/arena-deploy.log.
#
# WHY THIS DELEGATES RATHER THAN DRIVING COMPOSE ITSELF
#
# It used to run `docker compose up -d --build` directly, and that was wrong in
# two ways once there were two implementations to choose between.
#
# 1. It shipped the wrong Arena. ops/release.sh records the implementation in
#    backend/.release.env and layers it with --env-file; a bare `docker compose`
#    reads only backend/.env, so it fell back to the defaults in
#    docker-compose.yml -- claude-hq-arena-py:local, built from
#    backend/Dockerfile. An Arena released as Rust came back as PYTHON on the
#    next push, and silently, because the Python Arena is healthy and nothing
#    alerts on which one is running.
#
# 2. Layering the env file would NOT have been enough, and would have been
#    worse. ARENA_APP_IMAGE in that file is a stamped tag (date + commit), and
#    `up --build` rebuilds whatever tag it is handed -- so a new commit would
#    have been built into the PREVIOUS release's tag. `release.sh status` would
#    then report a version that is not what is running, and `release.sh
#    rollback`, which restores a previous image by tag without rebuilding,
#    would restore a tag that now holds the newer code: a rollback that
#    silently does nothing.
#
# So the release belongs to one script. release.sh already pulls ff-only,
# refuses a dirty tree, gates on GitHub CI for the commit, stamps a fresh
# version, builds, migrates, waits for health, and on failure rolls back to the
# last good IMAGE. This script's job is only to notice there is something to
# deploy, and to keep a log of it.
set -euo pipefail

DIR="${ARENA_DIR:-/root/claude-hq}"
BRANCH="${ARENA_BRANCH:-main}"
DOMAIN="${ARENA_DOMAIN:-}"
LOG="${ARENA_DEPLOY_LOG:-/var/log/arena-deploy.log}"

log() { printf '%s  %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1" >> "$LOG"; }

cd "$DIR"

git fetch origin "$BRANCH" --quiet
REMOTE=$(git rev-parse "origin/$BRANCH")
# Compare with what is RELEASED (the -<sha7> end of ARENA_VERSION), not the checkout:
# release.sh fast-forwards the checkout before its CI gate, so a push that arrives
# while CI is still running used to move HEAD, fail the gate, and then look
# "up to date" forever while the old image kept serving.
RELEASED=$(grep -E '^ARENA_VERSION=' backend/.release.env 2>/dev/null | tail -1 | sed -E 's/.*-([0-9a-f]{7,})$/\1/')
case "$REMOTE" in "${RELEASED:-none}"*) exit 0 ;; esac   # released == main: nothing to do

# Not released yet. Try at most every 5 minutes per commit (CI usually needs a few),
# and log the attempt once rather than on every tick.
TRIED="$DIR/.autodeploy-tried"
if [ -f "$TRIED" ] && [ "$(cut -d' ' -f1 "$TRIED")" = "$REMOTE" ] \
   && [ $(( $(date +%s) - $(cut -d' ' -f2 "$TRIED") )) -lt 300 ]; then
  exit 0
fi
FIRST=1; [ -f "$TRIED" ] && [ "$(cut -d' ' -f1 "$TRIED")" = "$REMOTE" ] && FIRST=0
echo "$REMOTE $(date +%s)" > "$TRIED"
LOCAL=$(git rev-parse "${RELEASED:-HEAD}" 2>/dev/null || git rev-parse HEAD)
[ "$FIRST" = 1 ] || log "retrying release of ${REMOTE:0:7} (released: ${RELEASED:-unknown})"

if [ "$FIRST" = 1 ]; then
  log "new commits ${LOCAL:0:7} -> ${REMOTE:0:7}"
  # -n 10, not "| head -10": under pipefail, head closing the pipe kills git log (SIGPIPE,
  # exit 141) and set -e then ends this script silently before it ever releases -- which
  # is what left the Arena stuck whenever more than ten commits were waiting.
  git log --oneline -n 10 "$LOCAL..$REMOTE" | while read -r line; do log "    $line"; done
fi

log "releasing with ops/release.sh"
if ARENA_BRANCH="$BRANCH" "$DIR/ops/release.sh" deploy >>"$LOG" 2>&1; then
  log "deployed ${REMOTE:0:7} OK"
  if [ -n "$DOMAIN" ]; then
    # The public path, through Caddy, which the in-container health check does
    # not cover: a healthy app behind a broken proxy is still an outage.
    if curl -fsS --max-time 10 "https://$DOMAIN/health" >/dev/null 2>&1; then
      log "  public endpoint healthy"
    else
      log "  WARNING: app is healthy but https://$DOMAIN/health is not answering"
    fi
  fi
else
  # release.sh has already restored the previous image and said so in the log
  # above; it only leaves the Arena down if there was no earlier release to go
  # back to, and says that too.
  log "ERROR: release failed -- see the ops/release.sh output above"
  exit 1
fi
