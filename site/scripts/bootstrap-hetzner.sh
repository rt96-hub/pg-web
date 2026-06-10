#!/bin/bash
set -euo pipefail

# Reusable bootstrap for a pg-web app on a fresh VPS (Hetzner etc.)
# Run from your laptop: bash site/scripts/bootstrap-hetzner.sh [ssh-alias] [domain]
#
# It will:
# - Install Docker on the remote via SSH
# - git clone the repo on the remote
# - Generate strong .env
# - Set production mode
# - Write a clean production docker-compose (Caddy on, DB ports off)
# - Build the image locally on the VPS if the prebuilt one isn't available
# - docker compose up -d
# - Run the first push with the in-container pg-web CLI
#
# This script itself is safe to commit. .env is gitignored.

SSH_HOST=${1:-hetzner}
DOMAIN=${2:-pg-web.dev}
REPO_URL=${3:-https://github.com/rt96-hub/pg-web.git}
APP_DIR=${4:-/opt/pg-web}

echo "==> Bootstrapping $DOMAIN on $SSH_HOST"

ssh "$SSH_HOST" 'echo "SSH connected as $(whoami) on $(hostname)"'

# Install Docker (idempotent)
ssh "$SSH_HOST" 'bash -s' << 'EOD'
set -euo pipefail
apt-get update -y >/dev/null
apt-get install -y ca-certificates curl gnupg git >/dev/null 2>&1 || true

install -m 0755 -d /etc/apt/keyrings 2>/dev/null || true
curl -fsSL https://download.docker.com/linux/ubuntu/gpg 2>/dev/null | gpg --dearmor -o /etc/apt/keyrings/docker.gpg 2>/dev/null || true
chmod a+r /etc/apt/keyrings/docker.gpg 2>/dev/null || true

echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" | tee /etc/apt/sources.list.d/docker.list > /dev/null 2>&1 || true

apt-get update -y >/dev/null 2>&1
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin >/dev/null 2>&1 || true

docker --version
docker compose version
EOD

# Clone/update repo
ssh "$SSH_HOST" "
  if [ -d $APP_DIR/.git ]; then
    cd $APP_DIR && git pull --ff-only || true
  else
    git clone $REPO_URL $APP_DIR
  fi
"

# Prepare site/ on remote
ssh "$SSH_HOST" "bash -s" << EOD
set -euo pipefail
cd $APP_DIR/site

# .env with strong password (if not present)
if [ ! -f .env ]; then
  echo "POSTGRES_PASSWORD=\$(openssl rand -base64 32)" > .env
  echo "Generated fresh .env"
fi

# production toml
sed -i 's/env  = "development"/env  = "production"/' pgweb.toml || true

# Clean production compose (overwrite with safe version)
cat > docker-compose.yml << 'COMPOSEEOF'
services:
  postgres:
    image: pgweb/postgres:latest
    restart: unless-stopped
    environment:
      POSTGRES_PASSWORD: \${POSTGRES_PASSWORD:-devpassword}
      POSTGRES_DB: app
    volumes:
      - pgdata:/var/lib/postgresql/data

  caddy:
    image: caddy:2
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      - postgres

volumes:
  pgdata:
  caddy_data:
  caddy_config:
COMPOSEEOF

echo "Production compose written"

# Bring up (will pull caddy; for pgweb image it will use local build if present)
docker compose up -d

echo "Waiting for init..."
sleep 12

# Try push; if image not present it will have failed earlier, but build is separate
docker compose exec -T postgres pg-web push --with-migrate || echo "Push will be retried after build if needed"

echo "Initial bootstrap phase complete on remote."
EOD

echo "==> Now ensuring the image is built on the remote (if the published one isn't available)"
ssh "$SSH_HOST" "
  if ! docker image inspect pgweb/postgres:latest >/dev/null 2>&1; then
    echo 'Image not present locally on VPS — building it now (5-12 min)...'
    cd $APP_DIR && bash scripts/build-image.sh
  else
    echo 'pgweb/postgres:latest already present on VPS'
  fi
"

echo "==> Final up + push"
ssh "$SSH_HOST" "
  cd $APP_DIR/site
  docker compose up -d
  sleep 10
  docker compose exec -T postgres pg-web push --with-migrate
  docker compose ps
"

echo ""
echo "==> DONE. Next:"
echo "   - Point DNS A for $DOMAIN → the VPS public IP"
echo "   - Visit https://$DOMAIN (Caddy will get the cert)"
echo "   - Future updates: ssh $SSH_HOST ; cd $APP_DIR/site ; git pull ; docker compose exec postgres pg-web push --with-migrate"
echo ""
echo "The script site/scripts/bootstrap-hetzner.sh is now in the repo for reuse on future VPSes."
