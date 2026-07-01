#!/usr/bin/env bash
# Install/refresh coturn on the box (the ICE relay fallback for symmetric-NAT / VPN peer pairs) and
# verify the relay's /turn endpoint mints a credential. Requires the shared secret to already be in
# /etc/smash-signaling.env -- run turn-secret.sh once first. Re-runnable.
#
# NOTE: the box-local firewall (ufw) is handled by install.sh; the Vultr CLOUD firewall must separately
# allow 3478/udp+tcp and 49160-49200/udp (install.sh prints this). A relayed match won't work until
# those cloud ports are open.
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

say "shipping coturn config + installer to $VPS"
ssh "$VPS" 'mkdir -p /root/deploy/coturn'
scp "$PROJ/deploy/coturn/turnserver.conf" "$VPS:/root/deploy/coturn/turnserver.conf"
scp "$PROJ/deploy/coturn/install.sh"      "$VPS:/root/deploy/coturn/install.sh"

say "running coturn installer on the box"
ssh "$VPS" 'bash /root/deploy/coturn/install.sh'

say "validating the TURN stack (deploy/scripts/turn-check.sh)"
"$(dirname "${BASH_SOURCE[0]}")/turn-check.sh"
