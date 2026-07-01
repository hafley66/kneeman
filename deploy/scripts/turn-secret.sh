#!/usr/bin/env bash
# One-time: generate a random shared secret for the TURN relay and point the relay at the TURN host,
# appended to the relay's EnvironmentFile on the box. The relay (TURN_SECRET) and coturn
# (static-auth-secret) read the SAME value; it never touches the repo or a client. Idempotent:
# rewrites both lines if already present. Re-run to rotate the secret (then re-run turn.sh + restart).
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

TURN_HOST="${TURN_HOST:-hafley.codes}"

say "writing TURN_HOST=$TURN_HOST + a fresh TURN_SECRET to /etc/smash-signaling.env on $VPS"
ssh "$VPS" "S=\$(openssl rand -hex 32); \
  touch /etc/smash-signaling.env; \
  sed -i '/^TURN_SECRET=/d;/^TURN_HOST=/d' /etc/smash-signaling.env; \
  printf 'TURN_HOST=%s\nTURN_SECRET=%s\n' '$TURN_HOST' \"\$S\" >> /etc/smash-signaling.env; \
  echo 'ok: env now carries TURN_HOST + TURN_SECRET'"
