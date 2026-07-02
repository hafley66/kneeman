# smash — task runner.  `just` to list, `just <cmd>` to run.
# (== npm scripts / Makefile, but sane)

godot := "godot"
proj  := justfile_directory()

# show this list
default:
    @just --list

# open the project in the Godot editor
edit:
    {{godot}} -e --path {{proj}}

# run the game (windowed)
run:
    {{godot}} --path {{proj}}

# run with verbose engine logging + rust backtraces
run-debug:
    RUST_BACKTRACE=1 {{godot}} --verbose --path {{proj}}

# tail the runtime log file (user:// logs)
logs:
    tail -f "$HOME/Library/Application Support/Godot/app_userdata/smash/logs/godot.log"

# nuke generated caches (.godot import cache + rust target)
clean:
    rm -rf {{proj}}/.godot {{proj}}/rust-sim/target

# run the main scene headless for N frames, fail on any script/scene error
check frames="120":
    {{godot}} --headless --path {{proj}} --quit-after {{frames}} 2>&1 | tee /dev/stderr | grep -qiE "SCRIPT ERROR|Parse Error|Failed to load" && exit 1 || echo "OK: clean"

# run a single scene file, e.g. `just scene scenes/game.tscn`
scene path:
    {{godot}} --path {{proj}} {{path}}

# --- rust sim (gdext) ---

# build the rust sim (debug), then re-scan so Godot registers the gdextension
rust: && import
    cd {{proj}}/rust-sim && cargo build

# editor import scan: registers .gdextension files + imports assets (run once after clone)
import:
    {{godot}} --headless --editor --quit --path {{proj}}

# fetch + convert sprite packs listed in tools/packs.toml -> assets/ + roster.json, then reimport
# so the new art lands in the .pck. Edit tools/packs.toml, run this, launch. See tools/packs.toml.
packs: && import
    python3 {{proj}}/tools/fetch_packs.py

# --- RoA workshop -> roster (search, QR login, headless download; see tools/roa_*.py) ---
# Full chain: roa-search -> steam-login (once) -> roa-get -> [[pack]] in packs.toml -> packs -> refs

tools_py := proj / "tools/.venv/bin/python3"

# ensure tools/.venv has the workshop deps (pinned in tools/requirements.txt)
_tools-venv:
    @[ -x {{tools_py}} ] || python3 -m venv {{proj}}/tools/.venv
    @{{tools_py}} -c "import steam.client, qrcode, PIL" 2>/dev/null || \
      {{proj}}/tools/.venv/bin/pip install -q -r {{proj}}/tools/requirements.txt

# search the RoA workshop for character packs, ranked by subscribers (stdlib, no auth)
roa-search query pages="1": _tools-venv
    {{tools_py}} {{proj}}/tools/roa_search.py "{{query}}" --pages {{pages}}

# one-time Steam login: pops a QR png, scan with the Steam mobile app. Token (a credential,
# keep it out of the repo) -> ~/.config/smash/steam_token.json
steam-login: _tools-venv
    {{tools_py}} {{proj}}/tools/steam_qr.py login

# download workshop item(s) headlessly -> tools/.cache/workshop/<id>/ (prints packs.toml lines).
# Pass several ids at once: one Steam session covers them all.
roa-get +ids: _tools-venv
    {{tools_py}} {{proj}}/tools/roa_get.py {{ids}}

# regenerate pose-capture ghost refs from an imported character's strips (after `just packs`)
refs char="assets/falcon": _tools-venv
    {{tools_py}} {{proj}}/tools/pose-capture/make_refs.py {{char}}

rust-release:
    cd {{proj}}/rust-sim && cargo build --release

rust-check:
    cd {{proj}}/rust-sim && cargo check

# rollback determinism gate: SyncTest rolls back every frame + checksums. Run after sim changes.
net-test:
    cd {{proj}}/rust-sim && cargo test -p smash_net

# --- signaling server (matchbox on the VPS, behind nginx /ws) ---

vps := "root@hafley.codes"

# --- pose capture (film a friend into a fighter) ---
# ship the capture app + nginx snippet; serves https://hafley.codes/game/poses/ (env-overridable)
poses-deploy:
    VPS={{vps}} PROJ={{proj}} deploy/scripts/poses.sh
# pull captured zips from the box -> ./captures and unzip (env: POSES_LOCAL, POSES_UNZIP)
poses-pull:
    VPS={{vps}} PROJ={{proj}} deploy/scripts/poses-pull.sh

# upload the nginx snippets + systemd unit, then (idempotently) install/start matchbox + nginx
vps-deploy:
    ssh {{vps}} 'mkdir -p /etc/nginx/snippets'
    scp deploy/nginx/matchbox-ws.conf {{vps}}:/etc/nginx/snippets/matchbox-ws.conf
    scp deploy/nginx/web.conf {{vps}}:/etc/nginx/snippets/web.conf
    scp deploy/nginx/godot.conf {{vps}}:/etc/nginx/snippets/godot.conf
    scp deploy/nginx/rtc.conf {{vps}}:/etc/nginx/snippets/rtc.conf
    scp deploy/systemd/matchbox.service {{vps}}:/etc/systemd/system/matchbox.service
    scp deploy/systemd/smash-signaling.service {{vps}}:/etc/systemd/system/smash-signaling.service
    rsync -az --delete --exclude target signaling/ {{vps}}:/root/smash-signaling-src/
    ssh {{vps}} 'bash -s' < deploy/setup-vps.sh

# VAPID contact, embedded in the signed push JWT. Override: `just vapid_subject=mailto:you@x.com vapid-keygen`
vapid_subject := "mailto:hafley66@gmail.com"

# Generate the VAPID keypair ON the VPS (idempotent — never overwrites an existing key) and write the
# systemd EnvironmentFile. The private key never leaves the box and is not in the repo; the matching
# public key is derived by the relay at boot and served from /vapid. Run once, then `signaling-deploy`.
vapid-keygen:
    ssh {{vps}} 'set -e; install -d -m700 /etc/smash-signaling; \
      KEY=/etc/smash-signaling/vapid.pem; \
      [ -f $KEY ] || openssl ecparam -genkey -name prime256v1 -noout -out $KEY; \
      chmod 600 $KEY; \
      printf "VAPID_PRIVATE_PEM=%s\nVAPID_SUBJECT=%s\n" $KEY "{{vapid_subject}}" > /etc/smash-signaling.env; \
      echo "vapid env written:"; cat /etc/smash-signaling.env'

# fully static x86_64 linux build of the relay, cross-compiled from this mac via zig (no Docker, no
# libssl on the box). musl => statically linked, runs on the ancient VPS with zero shared-lib deps.
# Setup once: `brew install zig && cargo install cargo-zigbuild && rustup target add x86_64-unknown-linux-musl`
signaling-bin:
    cd {{proj}}/signaling && cargo zigbuild --release --target x86_64-unknown-linux-musl

# deploy the relay by shipping the PREBUILT static binary — the 1-core/1GB box never compiles rust.
# Ships the binary + nginx snippets (rtc/ev/turn) + systemd unit and idempotently wires the includes.
# The recipe body is deploy/scripts/signaling.sh (VPS/PROJ/BIN overridable via env).
signaling-deploy: signaling-bin
    VPS={{vps}} PROJ={{proj}} deploy/scripts/signaling.sh

# --- TURN relay (coturn on the box; ICE fallback for symmetric-NAT / VPN peer pairs) ---

# One-time: generate a random shared secret + point the relay at the TURN host, appended to the relay's
# EnvironmentFile (never committed). Body: deploy/scripts/turn-secret.sh. Re-run to rotate.
turn-secret:
    VPS={{vps}} PROJ={{proj}} deploy/scripts/turn-secret.sh

# Install/refresh coturn (reads the secret from the env file) + verify /turn mints. Re-runnable.
# NOTE: also open 3478/udp+tcp and 49160-49200/udp on the Vultr CLOUD firewall. Body: deploy/scripts/turn.sh.
turn-deploy:
    VPS={{vps}} PROJ={{proj}} deploy/scripts/turn.sh

# Full TURN bring-up in one shot: relay (with /turn) + coturn + verify. Body: deploy/scripts/turn-up.sh.
# Prereq: run `just turn-secret` once first.
turn-up: signaling-bin
    VPS={{vps}} PROJ={{proj}} deploy/scripts/turn-up.sh

# Post-deploy TURN validation (mint + external STUN reachability + turnutils relay test). Run it from
# your laptop/CI (`brew install coturn` for turnutils) to prove the media range is externally reachable.
turn-check:
    VPS={{vps}} PROJ={{proj}} deploy/scripts/turn-check.sh

# tail the live netcode event firehose (client-stamped sid/cs + server-stamped t/cip/sip). Pretty via
# jq if present. This is the "stop guessing" view: watch phase/net/session_begin/hb lines in realtime.
analytics-tail:
    ssh {{vps}} 'touch /var/log/smash/ev.log; tail -n 40 -f /var/log/smash/ev.log' | (jq -c . 2>/dev/null || cat)

# tail the live signaling relay log (connect / matched / disconnect lines)
signaling-logs:
    ssh {{vps}} 'journalctl -u smash-signaling -n 60 -f'

# tail the signaling service log
vps-logs:
    ssh {{vps}} 'journalctl -u matchbox -n 60 -f'

# restart + status
vps-restart:
    ssh {{vps}} 'systemctl restart matchbox && systemctl --no-pager status matchbox'

# --- web frontend (wasm + canvas, via trunk) ---

# signaling room URL baked into the wasm build (override: `just matchbox_url=... web`)
matchbox_url := "wss://hafley.codes/ws?next=2"

# build the browser client to web/dist (release, asset URLs under /play/)
web:
    cd {{proj}}/web && MATCHBOX_URL="{{matchbox_url}}" trunk build --release --public-url /play/

# local dev: serve at http://localhost:8080 with autoreload. Open two tabs to pair.
# Override the signaling target at runtime via the page query `?url=ws://localhost:3536/x?next=2`.
web-dev:
    cd {{proj}}/web && trunk serve --open

# build + push web/dist to the VPS (/var/www/smash), then reload nginx. Run `vps-deploy` once first.
web-deploy: web
    rsync -az --delete {{proj}}/web/dist/ {{vps}}:/var/www/smash/
    ssh {{vps}} 'nginx -t && systemctl reload nginx'
    @echo "live at https://hafley.codes/play/"

# --- real Godot web export (served at /game/) ---

godot45 := "$HOME/godot45/Godot.app/Contents/MacOS/Godot"

# build the gdext sim for the web target (release). The nightly is PINNED by rust-sim/rust-toolchain.toml
# (no `+nightly` -- that would pull the rolling nightly and defeat the pin). Needs emsdk sourced + 4.5.
godot-wasm:
    cd {{proj}}/rust-sim && source ~/emsdk/emsdk_env.sh && \
      GODOT4_BIN="{{godot45}}" cargo build -p smash_sim -Zbuild-std \
      --target wasm32-unknown-emscripten --release

# export the Godot "Web" preset to build/web (runs the 4.5 editor headless), then drop in the push
# service worker + opt-in script (head_include loads push.js; sw.js registers at the /game/ scope)
godot-export: godot-wasm
    cd {{proj}} && mkdir -p build/web && source ~/emsdk/emsdk_env.sh && \
      "{{godot45}}" --headless --path . --export-release "Web" build/web/index.html
    cp {{proj}}/deploy/web/sw.js {{proj}}/deploy/web/push.js {{proj}}/deploy/web/turn-probe.js {{proj}}/build/web/
    # Precompress the big assets so the in-process server's ServeDir(.precompressed_gzip()) serves the
    # .gz directly (no per-request CPU). -k keeps the originals for clients that don't send gzip.
    cd {{proj}}/build/web && gzip -kf index.side.wasm smash_sim.wasm index.js index.wasm index.pck 2>/dev/null || true

# push build/web to the VPS (/var/www/smash-godot), reload nginx. Run `vps-deploy` once first.
godot-deploy: godot-export
    rsync -az --delete {{proj}}/build/web/ {{vps}}:/var/www/smash-godot/
    ssh {{vps}} 'nginx -t && systemctl reload nginx'
    @echo "live at https://hafley.codes/game/"

# THE deploy. One command: gate, build BOTH artifacts (game wasm + relay binary) here, push
# everything, restart. Idempotent + cached like a container build, without the daemon:
#   - cargo's target/ cache rebuilds only what changed (incremental); Cargo.lock pins every dep, so a
#     clean checkout builds the same bytes -- that's the "reproducible image" part.
#   - the relay ships as a STATIC musl binary: zero shared-lib deps, runs on the box like a FROM
#     scratch image. The 1-core/1GB box NEVER compiles; it only receives artifacts + restarts.
#   - rsync --delete delta-transfers only changed export files; re-running with no changes is a near
#     no-op (cargo no-ops, rsync sends nothing, restart is the only always-effect).
# Every step is safe to re-run. This is the whole deploy for the current (nginx-fronted) setup; after
# the TLS flip (plans/server-flip-runbook.md) the nginx steps drop and the relay serves everything.
ship: net-test signaling-bin godot-export
    # 1. static game export (wasm + gzipped assets) -> web root, delta transfer
    rsync -az --delete {{proj}}/build/web/ {{vps}}:/var/www/smash-godot/
    # 2. relay binary + its unit + the /game + /rtc nginx snippets
    scp {{proj}}/signaling/target/x86_64-unknown-linux-musl/release/smash-signaling {{vps}}:/root/.cargo/bin/smash-signaling.new
    scp {{proj}}/deploy/systemd/smash-signaling.service {{vps}}:/etc/systemd/system/smash-signaling.service
    scp {{proj}}/deploy/nginx/godot.conf {{proj}}/deploy/nginx/rtc.conf {{vps}}:/etc/nginx/snippets/
    # 3. atomically swap the binary, restart the relay, reload nginx (config-tested first)
    ssh {{vps}} 'mv /root/.cargo/bin/smash-signaling.new /root/.cargo/bin/smash-signaling && \
        systemctl daemon-reload && systemctl restart smash-signaling && \
        nginx -t && systemctl reload nginx && \
        echo "relay=$(systemctl is-active smash-signaling)"'
    @echo "shipped -> https://hafley.codes/game/"

# --- submodules / assets ---

# pull all .ext prior-art submodules
ext-init:
    git -C {{proj}} submodule update --init --recursive

# --- export (needs export templates + a preset named "macOS") ---
export-mac out="build/smash.app":
    {{godot}} --headless --path {{proj}} --export-release "macOS" {{out}}
