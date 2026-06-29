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

# upload the nginx snippet + systemd unit, then (idempotently) install/start matchbox
vps-deploy:
    ssh {{vps}} 'mkdir -p /etc/nginx/snippets'
    scp deploy/nginx/matchbox-ws.conf {{vps}}:/etc/nginx/snippets/matchbox-ws.conf
    scp deploy/systemd/matchbox.service {{vps}}:/etc/systemd/system/matchbox.service
    ssh {{vps}} 'bash -s' < deploy/setup-vps.sh

# tail the signaling service log
vps-logs:
    ssh {{vps}} 'journalctl -u matchbox -n 60 -f'

# restart + status
vps-restart:
    ssh {{vps}} 'systemctl restart matchbox && systemctl --no-pager status matchbox'

# --- submodules / assets ---

# pull all .ext prior-art submodules
ext-init:
    git -C {{proj}} submodule update --init --recursive

# --- export (needs export templates + a preset named "macOS") ---
export-mac out="build/smash.app":
    {{godot}} --headless --path {{proj}} --export-release "macOS" {{out}}
