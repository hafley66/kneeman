#!/usr/bin/env bash
# Post-deploy validation for the TURN stack. Three checks, loudest failure first:
#   1. /turn mints a credential            (relay endpoint + secret wired)
#   2. UDP 3478 answers a STUN Binding     (control port reachable from THIS host)
#   3. turnutils_uclient full relay test   (auth + media relay range end-to-end)
#
# Check 3 runs turnutils from wherever this script runs. Run it from your LAPTOP/CI (external) to prove
# the Vultr cloud firewall passes the media range; `brew install coturn` / `apt install coturn-utils`
# provides turnutils_uclient. If it's not present locally the script falls back to running it ON THE
# box (proves coturn + auth, but NOT external media reachability) and says so.
#
# Exit non-zero on any hard failure so a deploy script / CI gate can `&&` on it. This is the seed of a
# real e2e: today it's a smoke test; later it can assert relay RTT/loss thresholds.
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"
HOST="${TURN_HOST:-hafley.codes}"
FAIL=0

# 1. credential mint
say "check 1/3: GET https://$HOST/turn mints a credential"
CREDS="$(curl -fsS "https://$HOST/turn" || true)"
if [ -z "$CREDS" ]; then die "no credential from /turn (secret set? relay up?)"; fi
USERNAME="$(printf '%s' "$CREDS" | sed 's/.*"username":"\([0-9]*\)".*/\1/')"
CRED="$(printf '%s' "$CREDS" | sed 's/.*"credential":"\([^"]*\)".*/\1/')"
[ -n "$USERNAME" ] && [ -n "$CRED" ] || die "malformed /turn response: $CREDS"
echo "  ok: username=$USERNAME"

# 2. external STUN Binding reachability on 3478/udp (portable python probe)
say "check 2/3: UDP 3478 STUN Binding reachable from $(hostname -s)"
if python3 - "$HOST" <<'PY'
import socket,os,struct,sys
txid=os.urandom(12)
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(5)
try:
    s.sendto(struct.pack(">HHI",1,0,0x2112A442)+txid,(sys.argv[1],3478))
    d,_=s.recvfrom(1024)
    sys.exit(0 if struct.unpack(">H",d[:2])[0]==0x0101 else 1)
except Exception: sys.exit(1)
PY
then echo "  ok: 3478/udp answers"; else echo "  FAIL: no STUN reply on 3478/udp (cloud firewall?)"; FAIL=1; fi

# 3. full relay test with the real tool
say "check 3/3: turnutils_uclient allocation + relay"
if command -v turnutils_uclient >/dev/null; then
  WHERE="external ($(hostname -s))"
  RUN=(turnutils_uclient -t -u "$USERNAME" -w "$CRED" -y -c "$HOST")
  OUT="$("${RUN[@]}" 2>&1 || true)"
else
  WHERE="ON THE BOX (not external -- install turnutils locally to test the cloud firewall's media range)"
  # Relay peer must be the PUBLIC host: turnserver.conf denied-peer-ip blocks loopback/private ranges.
  OUT="$(ssh "$VPS" "turnutils_uclient -t -u '$USERNAME' -w '$CRED' -y -c '$HOST'" 2>&1 || true)"
fi
if printf '%s' "$OUT" | grep -q "Total lost packets 0"; then
  echo "  ok [$WHERE]: $(printf '%s' "$OUT" | grep -E 'Total lost packets|round trip' | tr '\n' ' ')"
else
  echo "  FAIL [$WHERE]:"; printf '%s\n' "$OUT" | tail -6; FAIL=1
fi

[ "$FAIL" -eq 0 ] && say "TURN validation PASSED" || die "TURN validation FAILED (see above)"
