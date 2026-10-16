#!/usr/bin/env bash
#
# One command to ship the Arena: test, build, stamp a version, deploy, verify,
# and roll back to the previous build if it is not healthy.
#
#   ops/release.sh check                  run every test suite locally (before you merge)
#   ops/release.sh deploy                 ship origin/main (Python Arena)
#   ARENA_IMPL=rs ops/release.sh deploy   ship origin/main as the Rust Arena
#   ops/release.sh rollback               go back to the previous release (no rebuild)
#   ops/release.sh status                 what is running, and the release history
#
# Run deploy/rollback/status on the Arena host, from the repo checkout. Each
# release is a tagged image (claude-hq-arena-<impl>:<version>, the version being
# the UTC date + commit), so a rollback just starts the previous image again.
# The last KEEP releases per implementation are kept; older images are pruned.
#
# Options (environment):
#   ARENA_IMPL=py|rs        which Arena to run (default: whatever runs now, else py)
#   ARENA_BRANCH=main       branch to release
#   ARENA_SKIP_CI=1         deploy even if GitHub CI has not passed for the commit
#   ARENA_CI_WAIT=300       seconds to wait for CI that is still running
#   ARENA_SKIP_PULL=1       release the checkout as it is (used by the CI smoke test)
#   ARENA_KEEP=3            releases to keep per implementation
#
# A rollback restores code, not data: if the release ran a migration, the older
# build meets a newer schema. Alembic migrations here only ever add, so that is
# normally fine; the script says so when the schema moved.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKEND="$REPO/backend"
STATE="$BACKEND/.release.env"         # what compose should run now (git-ignored)
HISTORY="$BACKEND/.releases"          # one line per release, newest last (git-ignored)
BRANCH="${ARENA_BRANCH:-main}"
KEEP="${ARENA_KEEP:-3}"
CI_WAIT="${ARENA_CI_WAIT:-300}"
GH_REPO="${ARENA_GH_REPO:-RoString36222/claude-hq}"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
ok()   { printf '    \033[32m✓\033[0m %s\n' "$1"; }
warn() { printf '    \033[33m!\033[0m %s\n' "$1"; }
die()  { printf '    \033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }

# docker compose with the release state layered over .env (both optional).
dc() {
  local args=()
  [[ -f "$BACKEND/.env" ]] && args+=(--env-file "$BACKEND/.env")
  [[ -f "$STATE" ]] && args+=(--env-file "$STATE")
  ( cd "$BACKEND" && docker compose "${args[@]}" "$@" )
}

current() { [[ -f "$STATE" ]] && grep -E "^$1=" "$STATE" | tail -1 | cut -d= -f2- || true; }

write_state() {   # impl version
  local impl="$1" version="$2"
  {
    echo "# Written by ops/release.sh: the release compose runs now."
    echo "ARENA_IMPL=$impl"
    echo "ARENA_VERSION=$version"
    echo "ARENA_APP_IMAGE=claude-hq-arena-$impl:$version"
    echo "ARENA_PY_IMAGE=claude-hq-arena-py:$version"
    if [[ "$impl" == rs ]]; then
      echo "ARENA_BUILD_CONTEXT=.."
      echo "ARENA_DOCKERFILE=backend-rs/Dockerfile"
    else
      echo "ARENA_BUILD_CONTEXT=."
      echo "ARENA_DOCKERFILE=Dockerfile"
    fi
  } > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

healthy() { dc exec -T app arena-health >/dev/null 2>&1; }

# The version the running container was built as (the app port is never published,
# and the Rust image has no python or curl, so ask the environment).
running_version() { dc exec -T app printenv ARENA_VERSION 2>/dev/null | tr -d '\r' || true; }

wait_healthy() {  # expected version (optional)
  local want="${1:-}"
  for _ in $(seq 1 24); do
    sleep 5
    if healthy; then
      if [[ -n "$want" ]]; then
        local got; got=$(running_version)
        [[ "$got" == "$want" ]] || continue
      fi
      return 0
    fi
  done
  return 1
}

ci_passed() {  # sha -> 0 passed, 1 failed, 2 GitHub unreachable, 3 still running
  local sha="$1" body
  body=$(curl -fsS --max-time 20 -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$GH_REPO/commits/$sha/check-runs?per_page=100") || return 2
  python3 - "$body" <<'PY'
import json, sys
runs = json.loads(sys.argv[1]).get("check_runs", [])
if not runs:
    print("    no CI runs for this commit yet, waiting"); sys.exit(3)
done = [r for r in runs if r.get("status") == "completed"]
failed = [r for r in done if r.get("conclusion") not in ("success", "skipped", "neutral")]
pending = [r["name"] for r in runs if r.get("status") != "completed"]
if pending and not failed:
    print("    waiting on: %s" % ", ".join(pending)); sys.exit(3)
for r in runs:
    print("    %-10s %s" % (r["name"], r.get("conclusion") or r.get("status")))
sys.exit(1 if failed else 0)
PY
}

prune() {  # impl
  local impl="$1" keep
  keep=$(grep -E " $impl " "$HISTORY" 2>/dev/null | tail -n "$KEEP" | awk '{print $2}' || true)
  docker image ls "claude-hq-arena-$impl" --format '{{.Tag}}' | while read -r tag; do
    [[ "$tag" == local || "$tag" == "<none>" ]] && continue
    grep -qx "$tag" <<<"$keep" || docker image rm "claude-hq-arena-$impl:$tag" >/dev/null 2>&1 || true
  done
}

cmd_check() {
  say "Python unit tests (HQ)"
  ( cd "$REPO" && python3 -m unittest discover -s tests -q ) && ok "HQ"
  say "Arena backend tests (Python)"
  ( cd "$BACKEND" && uv run pytest -q ) && ok "backend"
  say "Arena backend tests (Rust) + clippy"
  ( cd "$REPO/backend-rs" && cargo test --locked -q && cargo clippy --locked --all-targets -q -- -D warnings ) && ok "backend-rs"
  ok "all green: safe to merge"
}

cmd_deploy() {
  local impl="${ARENA_IMPL:-$(current ARENA_IMPL)}"; impl="${impl:-py}"
  [[ "$impl" == py || "$impl" == rs ]] || die "ARENA_IMPL must be py or rs"
  local prev_version prev_impl
  prev_version=$(current ARENA_VERSION); prev_impl=$(current ARENA_IMPL)

  if [[ "${ARENA_SKIP_PULL:-}" != 1 ]]; then
    say "Fetching $BRANCH"
    [[ -z "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]] || die "local changes in $REPO; commit or stash them first"
    git -C "$REPO" fetch origin "$BRANCH" --quiet
    git -C "$REPO" merge --ff-only "origin/$BRANCH" --quiet || die "$BRANCH has diverged from origin; fix by hand"
  fi
  local sha; sha=$(git -C "$REPO" rev-parse HEAD)
  ok "at ${sha:0:7}: $(git -C "$REPO" log -1 --format=%s)"

  if [[ "${ARENA_SKIP_CI:-}" != 1 ]]; then
    say "Checking GitHub CI for ${sha:0:7}"
    # A deploy right after a merge lands while CI is still running: wait for it
    # rather than fail (autodeploy would otherwise not retry for 5 minutes).
    local waited=0 rc
    while :; do
      rc=0; ci_passed "$sha" || rc=$?
      [[ "$rc" == 3 && "$waited" -lt "$CI_WAIT" ]] || break
      sleep 20; waited=$((waited + 20))
    done
    case "$rc" in
      0) ok "CI passed" ;;
      3) die "CI still running after ${CI_WAIT}s (ARENA_SKIP_CI=1 to override)" ;;
      2) die "could not reach GitHub to check CI (ARENA_SKIP_CI=1 to override)" ;;
      *) die "CI failed for this commit (ARENA_SKIP_CI=1 to override)" ;;
    esac
  fi

  local version; version="$(date -u +%Y.%m.%d)-${sha:0:7}"
  if [[ "$version" == "$prev_version" && "$impl" == "$prev_impl" ]] && healthy; then
    ok "$version ($impl) is already running"; return 0
  fi

  say "Building $impl $version"
  write_state "$impl" "$version"
  dc build app || { restore "$prev_impl" "$prev_version"; die "build failed"; }
  if [[ "$impl" == rs ]]; then
    dc --profile tools build migrate || { restore "$prev_impl" "$prev_version"; die "build failed (migrate image)"; }
    say "Migrating the database (Alembic, Python image)"
    dc --profile tools run --rm -T migrate || { restore "$prev_impl" "$prev_version"; die "migration failed"; }
  fi

  say "Starting $impl $version"
  dc up -d --no-build app
  if wait_healthy "$version"; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $version $impl ${sha:0:7}" >> "$HISTORY"
    ok "healthy, running $(running_version)"
    prune "$impl"
    say "Released $version ($impl)"
  else
    warn "not healthy after 2 minutes; logs:"
    dc logs --tail 40 app || true
    if [[ -n "$prev_version" ]]; then
      restore "$prev_impl" "$prev_version"
      die "release failed; rolled back to $prev_version ($prev_impl)"
    fi
    die "release failed and there is no previous release to go back to"
  fi
}

restore() {  # impl version: start an earlier release's image again (no rebuild)
  local impl="$1" version="$2"
  [[ -n "$version" ]] || { rm -f "$STATE"; return 0; }
  docker image inspect "claude-hq-arena-$impl:$version" >/dev/null 2>&1 \
    || die "image claude-hq-arena-$impl:$version is gone; cannot roll back without rebuilding"
  say "Rolling back to $version ($impl)"
  write_state "$impl" "$version"
  dc up -d --no-build app
  if wait_healthy "$version"; then ok "rolled back, running $(running_version)"
  else warn "the rollback is not healthy either. If a migration ran, the old build may not know the new schema: roll FORWARD with ops/release.sh deploy"
  fi
}

cmd_rollback() {
  [[ -f "$HISTORY" ]] || die "no release history yet"
  local cur cur_impl prev
  cur=$(current ARENA_VERSION); cur_impl=$(current ARENA_IMPL)
  # the newest release that is not the one running (same version, other impl counts)
  prev=$(awk -v cur="$cur" -v ci="$cur_impl" '!($2 == cur && $3 == ci) && $5 != "rollback"' "$HISTORY" | tail -1)
  [[ -n "$prev" ]] || die "no earlier release to roll back to"
  restore "$(awk '{print $3}' <<<"$prev")" "$(awk '{print $2}' <<<"$prev")"
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $(awk '{print $2, $3, $4}' <<<"$prev") rollback" >> "$HISTORY"
}

cmd_status() {
  say "Running"
  if [[ -f "$STATE" ]]; then ok "$(current ARENA_VERSION) ($(current ARENA_IMPL))"; else warn "no release yet (plain docker compose)"; fi
  if healthy; then ok "healthy, running $(running_version)"; else warn "not healthy (or not running)"; fi
  say "History (newest last)"
  if [[ -f "$HISTORY" ]]; then tail -n 10 "$HISTORY" | sed 's/^/    /'; else echo "    none"; fi
  say "Images kept"
  docker image ls --format '    {{.Repository}}:{{.Tag}}  {{.CreatedSince}}  {{.Size}}' | grep claude-hq-arena || echo "    none"
}

case "${1:-}" in
  check)    cmd_check ;;
  deploy)   cmd_deploy ;;
  rollback) cmd_rollback ;;
  status)   cmd_status ;;
  *) sed -n '3,12p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
