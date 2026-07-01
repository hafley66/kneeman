# Server flip runbook (steps 3-4): nginx + matchbox -> one binary

Cutover from the 3-process setup (nginx + matchbox + smash-signaling behind nginx) to the single
axum binary terminating TLS itself. Prereqs: steps 1-2 shipped (binary already live in mode 1).

Box: Azure VM, 1 vCPU / 955 MB, root. Binary at `/root/.cargo/bin/smash-signaling`. Domain
`hafley.codes`. The binary runs as root, so it can bind :80/:443 directly.

## What breaks if this goes wrong

The binary obtains its Let's Encrypt cert on first boot via **TLS-ALPN-01 on :443**. That needs :443
free (nginx stopped) and reachable. Between "start in TLS mode" and "cert issued" (~10-90s) HTTPS is
down. So: prove the mechanism against LE **staging** first, keep nginx installed for instant
rollback, and do the prod flip in a low-traffic window.

## Stage A — prove ACME works (no downtime)

Test issuance on a scratch port against LE staging, while nginx keeps serving :443 untouched.

```
# on the box, one-shot manual run (NOT via systemd), staging dir, high port:
TLS_DOMAINS=hafley.codes ACME_PRODUCTION= ACME_CACHE_DIR=/tmp/acme-stg \
  GAME_DIR=/var/www/smash-godot /root/.cargo/bin/smash-signaling
```

Wait — this still binds :443 internally (ALPN-01), which nginx holds. So Stage A can only PROVE the
code path, not issue while nginx owns :443. Two real options:

- **A1 (recommended):** accept a short window. Do Stage B directly in a quiet minute; the staging
  rehearsal below runs with nginx briefly stopped.
- **A2:** move ACME to HTTP-01 on :80 via a sidecar — not worth it for a hobby box.

Staging rehearsal (brief :443 blip, throwaway cert):
```
systemctl stop nginx
TLS_DOMAINS=hafley.codes ACME_PRODUCTION= ACME_CACHE_DIR=/tmp/acme-stg \
  GAME_DIR=/var/www/smash-godot timeout 120 /root/.cargo/bin/smash-signaling &
# watch logs for "acme: ... Ready" / a cert in /tmp/acme-stg. Then:
curl -vk https://hafley.codes/status            # TLS handshake succeeds (untrusted staging CA = ok)
kill %1; systemctl start nginx                  # back to mode 1
```
Green here = the ACME + bind + serve path works end to end.

## Stage B — the prod flip

```
# 1. fresh export WITH gzipped assets (justfile godot-export now gzips), pushed to the static root:
just godot-export
rsync -az --delete build/web/ root@hafley.codes:/var/www/smash-godot/

# 2. put the real-TLS env in place (persists the cert under StateDirectory):
ssh root@hafley.codes 'install -d -m700 /var/lib/smash-signaling/acme; \
  printf "TLS_DOMAINS=hafley.codes\nACME_PRODUCTION=1\n" >> /etc/smash-signaling.env'

# 3. deploy the flip-ready unit + latest binary (no on-box compile):
just signaling-deploy        # scp binary + unit, daemon-reload, restart -- STILL mode 1 here

# 4. free :80/:443 and restart into TLS mode:
ssh root@hafley.codes 'systemctl stop nginx matchbox && systemctl restart smash-signaling'

# 5. verify (cert issues within ~90s of restart):
curl -sI https://hafley.codes/game/                     # 200, served by the binary
curl -sI -H "Accept-Encoding: gzip" https://hafley.codes/game/index.side.wasm  # content-encoding: gzip
curl -s  https://hafley.codes/status | head -c 80       # relay status JSON
# open the game, Find Match on two devices -> Running.
```

### Rollback (if the cert won't issue or /game 500s)

```
ssh root@hafley.codes 'systemctl stop smash-signaling; \
  sed -i "/TLS_DOMAINS/d;/ACME_PRODUCTION/d" /etc/smash-signaling.env; \
  systemctl start nginx matchbox smash-signaling'   # back to mode 1 in seconds
```

Because the flip is env-only, rollback is deleting two env lines and starting nginx.

## Stage C — cleanup (step 4, after a day of green)

- `systemctl disable nginx matchbox` (leave installed a week for fast rollback, then remove).
- Delete `deploy/nginx/*`, `deploy/systemd/matchbox.service`, the matchbox bits of `setup-vps.sh`.
- `setup-vps.sh` shrinks to: install binary, install unit, `install -d` the state dirs, enable+start.
- Retire `/var/www/smash` (the canvas `/play/` build) and the `web/` matchbox client if unused.
- `just ship` drops `vps-deploy`; becomes: net-test -> export+gzip -> rsync static -> signaling-deploy.

## Open risks

- **:443 cold-cert window** on the flip restart (~10-90s no HTTPS). Do it off-peak. Unavoidable
  without pre-seeding the cert.
- **LE rate limits**: 5 certs/domain/week. The staging rehearsal uses the staging endpoint, so it
  doesn't burn the prod quota. Don't loop prod restarts.
- **Single point of failure**: one process now serves TLS + static + relay. A panic takes everything
  down (systemd `Restart=on-failure` catches it). Acceptable for a hobby box; was already true of nginx.
