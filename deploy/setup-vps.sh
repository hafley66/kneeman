#!/usr/bin/env bash
# Idempotent VPS setup for the signaling server. Run via `just vps-deploy`, which scp's
# deploy/nginx/matchbox-ws.conf + deploy/systemd/matchbox.service first, then pipes this in.
# Assumes Ubuntu + nginx already serving the 443 hafley.codes block (with the Let's Encrypt cert).
set -euo pipefail

# 1. matchbox_server binary — install once if missing (rust toolchain, then cargo install).
if [ ! -x "$HOME/.cargo/bin/matchbox_server" ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    . "$HOME/.cargo/env"
  fi
  cargo install matchbox_server
fi

# 2. nginx — include the /ws reverse-proxy snippet inside the 443 server block (once).
SITE=/etc/nginx/sites-enabled/default
if ! grep -q 'snippets/matchbox-ws.conf' "$SITE"; then
  sed -i '/server_name www.hafley.codes hafley.codes; # managed by Certbot/a\    include snippets/matchbox-ws.conf;' "$SITE"
fi

# 3. systemd — load + enable + start the service.
systemctl daemon-reload
systemctl enable --now matchbox

# 4. validate nginx, then reload.
nginx -t
systemctl reload nginx

echo "OK: matchbox on 127.0.0.1:3536, proxied at wss://hafley.codes/ws"
