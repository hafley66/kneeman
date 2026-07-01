#!/usr/bin/env bash
# Idempotent coturn install for the smash TURN relay. Run on the box (root@hafley.codes).
# Reads TURN_SECRET from /etc/smash-signaling.env so the relay and coturn share one secret; the
# secret is never written to the repo. Safe to re-run — it re-substitutes the config and restarts.
set -euo pipefail

ENV_FILE=/etc/smash-signaling.env
CONF_SRC=/root/deploy/coturn/turnserver.conf   # scp'd here by `just turn-deploy`
CONF_DST=/etc/turnserver.conf
MIN_PORT=49160
MAX_PORT=49200

# 1. Pull the shared secret the relay already uses (set by `just turn-secret`).
if [ ! -f "$ENV_FILE" ] || ! grep -q '^TURN_SECRET=' "$ENV_FILE"; then
  echo "FATAL: TURN_SECRET= not found in $ENV_FILE. Run 'just turn-secret' first." >&2
  exit 1
fi
SECRET=$(grep '^TURN_SECRET=' "$ENV_FILE" | head -1 | cut -d= -f2-)

# 2. Install coturn (no-op if already present).
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq coturn

# 3. Install config with the real secret substituted in (never committed).
sed "s|^static-auth-secret=.*|static-auth-secret=${SECRET}|" "$CONF_SRC" > "$CONF_DST"
chmod 600 "$CONF_DST"

# 4. Debian ships coturn disabled by default; flip the enable flag.
if [ -f /etc/default/coturn ]; then
  sed -i 's|^#\?TURNSERVER_ENABLED=.*|TURNSERVER_ENABLED=1|' /etc/default/coturn
  grep -q '^TURNSERVER_ENABLED=1' /etc/default/coturn || echo 'TURNSERVER_ENABLED=1' >> /etc/default/coturn
fi

# 5. Open the box-local firewall (ufw) if it's active. Harmless no-op otherwise.
if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
  ufw allow 3478/udp
  ufw allow 3478/tcp
  ufw allow ${MIN_PORT}:${MAX_PORT}/udp
fi

systemctl enable coturn
systemctl restart coturn
sleep 1
systemctl --no-pager status coturn | head -5

echo
echo "==> coturn installed. STILL REQUIRED on the Vultr CLOUD firewall (web console), if enabled:"
echo "      3478/udp  3478/tcp  ${MIN_PORT}-${MAX_PORT}/udp"
echo "    Verify externally after opening:  turnutils_uclient -v -u <user> -w <pw> hafley.codes"
