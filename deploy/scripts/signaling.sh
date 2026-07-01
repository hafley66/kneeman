#!/usr/bin/env bash
# Deploy the signaling relay: ship the prebuilt static binary + its nginx snippets + systemd unit,
# idempotently wire the snippet `include`s into the 443 server block, then atomically swap the binary
# and restart. The 1-core box never compiles rust -- build with `just signaling-bin` first (musl).
#
# nginx snippets, each `include`d after the previous one so order is stable:
#   rtc.conf   wss /rtc relay          (base; assumed already included)
#   ev.conf    POST /ev firehose sink
#   turn.conf  GET  /turn cred mint
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

[ -f "$BIN" ] || die "relay binary not found at $BIN -- run 'just signaling-bin' first"

say "shipping relay binary + nginx snippets + unit to $VPS"
scp "$BIN" "$VPS:/root/.cargo/bin/smash-signaling.new"
scp "$PROJ/deploy/nginx/rtc.conf"  "$VPS:/etc/nginx/snippets/rtc.conf"
scp "$PROJ/deploy/nginx/ev.conf"   "$VPS:/etc/nginx/snippets/ev.conf"
scp "$PROJ/deploy/nginx/turn.conf" "$VPS:/etc/nginx/snippets/turn.conf"
scp "$PROJ/deploy/systemd/smash-signaling.service" "$VPS:/etc/systemd/system/smash-signaling.service"

say "ensuring nginx snippet includes + swapping binary + restarting"
ssh "$VPS" 'bash -s' <<'REMOTE'
set -euo pipefail
SITE=/etc/nginx/sites-enabled/default

# Idempotently insert `include snippets/<new>;` right after an existing include line.
ensure_include() {
  local after="$1" new="$2"
  grep -q "snippets/$new" "$SITE" && return 0
  sed -i "/include snippets\/$after;/a\\    include snippets/$new;" "$SITE"
  echo "  + included snippets/$new (after $after)"
}
ensure_include rtc.conf ev.conf
ensure_include ev.conf  turn.conf

mv /root/.cargo/bin/smash-signaling.new /root/.cargo/bin/smash-signaling
systemctl daemon-reload
nginx -t
systemctl reload nginx
systemctl restart smash-signaling
systemctl --no-pager status smash-signaling | head -4
REMOTE
