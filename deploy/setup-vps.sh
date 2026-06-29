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

# 1b. smash-signaling relay — build/refresh from the source rsync'd by `just vps-deploy`.
. "$HOME/.cargo/env" 2>/dev/null || true
if [ -d "$HOME/smash-signaling-src" ]; then
  cargo install --path "$HOME/smash-signaling-src" --force
fi

# 2. nginx — include the /ws proxy + /play static snippets inside the 443 server block (once each).
SITE=/etc/nginx/sites-enabled/default
ANCHOR='/server_name www.hafley.codes hafley.codes; # managed by Certbot/'
if ! grep -q 'snippets/matchbox-ws.conf' "$SITE"; then
  sed -i "${ANCHOR}a\\    include snippets/matchbox-ws.conf;" "$SITE"
fi
if ! grep -q 'snippets/web.conf' "$SITE"; then
  sed -i "${ANCHOR}a\\    include snippets/web.conf;" "$SITE"
fi
if ! grep -q 'snippets/godot.conf' "$SITE"; then
  sed -i "${ANCHOR}a\\    include snippets/godot.conf;" "$SITE"
fi
if ! grep -q 'snippets/rtc.conf' "$SITE"; then
  sed -i "${ANCHOR}a\\    include snippets/rtc.conf;" "$SITE"
fi
mkdir -p /var/www/smash /var/www/smash-godot

# 3. systemd — load + enable + start the services.
systemctl daemon-reload
systemctl enable --now matchbox
systemctl enable --now smash-signaling
systemctl restart smash-signaling   # pick up a freshly rebuilt binary

# 4. validate nginx, then reload.
nginx -t
systemctl reload nginx

echo "OK: matchbox :3536 (wss /ws), smash-signaling :3537 (wss /rtc), canvas /play/, Godot /game/"
