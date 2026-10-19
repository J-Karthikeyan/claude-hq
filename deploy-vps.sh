#!/usr/bin/env bash
# Bootstrap the Claude HQ Arena backend on a fresh Ubuntu/Debian VPS.
#
#   sudo ARENA_DOMAIN=arena.example.com bash deploy-vps.sh
#
# Idempotent: safe to re-run after changing .env or pulling new code.
set -euo pipefail

DOMAIN="${ARENA_DOMAIN:?set ARENA_DOMAIN, e.g. ARENA_DOMAIN=arena.heshwa.dev}"
REPO="${ARENA_REPO:-https://github.com/RoString36222/claude-hq.git}"
DIR="${ARENA_DIR:-/opt/claude-hq}"

say() { printf '\n\033[1;34m==>\033[0m %s\n' "$1"; }

[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }

say "Checking DNS for $DOMAIN"
IP_HERE=$(curl -fsS --max-time 10 https://api.ipify.org || echo "?")
IP_DNS=$(getent hosts "$DOMAIN" | awk '{print $1}' | head -1 || echo "")
echo "    this server : $IP_HERE"
echo "    $DOMAIN -> ${IP_DNS:-<no A record>}"
if [ "$IP_DNS" != "$IP_HERE" ]; then
  echo "    WARNING: they do not match. Let's Encrypt will fail until the A record"
  echo "    points here and has propagated. Continuing anyway."
fi

say "Installing Docker"
if ! command -v docker >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq ca-certificates curl git ufw
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] \
https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -qq
  apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable --now docker
fi
docker --version

say "Firewall: allow SSH, HTTP, HTTPS only"
ufw allow 22/tcp  >/dev/null 2>&1 || true
ufw allow 80/tcp  >/dev/null 2>&1 || true
ufw allow 443/tcp >/dev/null 2>&1 || true
ufw --force enable >/dev/null 2>&1 || true
ufw status | head -6

say "Fetching the code into $DIR"
if [ -d "$DIR/.git" ]; then
  git -C "$DIR" pull --ff-only
else
  git clone --depth 1 "$REPO" "$DIR"
fi

cd "$DIR/backend"

if [ ! -f .env ]; then
  say "Creating .env — you need your GitHub OAuth app values"
  echo "    Callback URL to register: https://$DOMAIN/v1/auth/github/callback"
  printf '    GitHub Client ID: '; read -r CID
  printf '    GitHub Client Secret: '; read -rs CSEC; echo
  cat > .env <<EOF
ARENA_SECRET_KEY=$(openssl rand -base64 48 | tr -d '\n/+=')
ARENA_GITHUB_CLIENT_ID=$CID
ARENA_GITHUB_CLIENT_SECRET=$CSEC
EOF
  chmod 600 .env
  echo "    wrote .env (0600)"
else
  echo "    .env already present, keeping it"
fi

say "Starting (Caddy will obtain the TLS certificate)"
ARENA_DOMAIN="$DOMAIN" docker compose up -d --build
sleep 10
docker compose ps

say "Verifying"
for i in $(seq 1 12); do
  if curl -fsS --max-time 10 "https://$DOMAIN/health" 2>/dev/null; then
    printf '\n\n\033[1;32m✓ Arena is live at https://%s\033[0m\n\n' "$DOMAIN"
    echo "Next:"
    echo "  1. GitHub OAuth callback -> https://$DOMAIN/v1/auth/github/callback"
    echo "  2. Friends: Claude HQ -> Arena tab -> server URL -> https://$DOMAIN"
    echo
    echo "Logs:    docker compose -f $DIR/backend/docker-compose.yml logs -f"
    echo "Update:  cd $DIR && git pull && cd backend && ARENA_DOMAIN=$DOMAIN docker compose up -d --build"
    exit 0
  fi
  echo "    not ready yet ($i/12), waiting 10s — TLS issuance takes a moment"
  sleep 10
done

echo
echo "Did not get a healthy response. Check:"
echo "  docker compose -f $DIR/backend/docker-compose.yml logs caddy | tail -30"
echo "  docker compose -f $DIR/backend/docker-compose.yml logs app | tail -30"
exit 1
