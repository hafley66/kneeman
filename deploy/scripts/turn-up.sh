#!/usr/bin/env bash
# Full TURN bring-up in one shot: deploy the relay (with the /turn route + nginx snippet), then install
# coturn and verify the credential mint. Assumes the binary is already built (`just signaling-bin`) and
# the secret is already set (`just turn-secret`, run once). This is the "whole thing" as a script; the
# justfile `turn-up` recipe just builds the binary and calls it.
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"
HERE="$(dirname "${BASH_SOURCE[0]}")"

# 1. relay: binary + /turn route + nginx /turn snippet.
"$HERE/signaling.sh"

# 2. coturn: install/refresh + verify /turn mints.
"$HERE/turn.sh"

cat <<EOF

$(say "relay + coturn up, /turn verified.")
Remaining, by hand:
  1. Open the Vultr CLOUD firewall: 3478/udp 3478/tcp 49160-49200/udp
  2. Ship the client that fetches /turn:   just godot-deploy
  3. Re-test on two devices; tail the firehose:   just analytics-tail
     expect: turn ok:true  ->  gather ... complete (relay candidate)  ->  conn connected  ->  chan open
EOF
