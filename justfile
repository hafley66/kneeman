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

rust-release:
    cd {{proj}}/rust-sim && cargo build --release

rust-check:
    cd {{proj}}/rust-sim && cargo check

# rollback determinism gate: SyncTest rolls back every frame + checksums. Run after sim changes.
net-test:
    cd {{proj}}/rust-sim && cargo test -p smash_net

# --- signaling server (matchbox on the VPS, behind nginx /ws) ---

vps := "root@hafley.codes"

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

# rebuild + restart just the WebRTC signaling relay (no full setup rerun). Run `vps-deploy` once first.
signaling-deploy:
    rsync -az --delete --exclude target signaling/ {{vps}}:/root/smash-signaling-src/
    ssh {{vps}} 'source $HOME/.cargo/env && cargo install --path /root/smash-signaling-src --force && systemctl restart smash-signaling && systemctl --no-pager status smash-signaling | head -4'

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

# build the gdext sim for the web target (release). Needs emsdk sourced + nightly + 4.5 binary.
godot-wasm:
    cd {{proj}}/rust-sim && source ~/emsdk/emsdk_env.sh && \
      GODOT4_BIN="{{godot45}}" cargo +nightly build -p smash_sim -Zbuild-std \
      --target wasm32-unknown-emscripten --release

# export the Godot "Web" preset to build/web (runs the 4.5 editor headless)
godot-export: godot-wasm
    cd {{proj}} && mkdir -p build/web && source ~/emsdk/emsdk_env.sh && \
      "{{godot45}}" --headless --path . --export-release "Web" build/web/index.html

# push build/web to the VPS (/var/www/smash-godot), reload nginx. Run `vps-deploy` once first.
godot-deploy: godot-export
    rsync -az --delete {{proj}}/build/web/ {{vps}}:/var/www/smash-godot/
    ssh {{vps}} 'nginx -t && systemctl reload nginx'
    @echo "live at https://hafley.codes/game/"

# ship the game: gate on the rollback determinism test, then compile + export + deploy.
# Use this (not raw godot-deploy) for any change that touches the sim or netplay path.
ship: net-test godot-deploy

# --- submodules / assets ---

# pull all .ext prior-art submodules
ext-init:
    git -C {{proj}} submodule update --init --recursive

# --- export (needs export templates + a preset named "macOS") ---
export-mac out="build/smash.app":
    {{godot}} --headless --path {{proj}} --export-release "macOS" {{out}}
