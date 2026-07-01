#!/usr/bin/env bash
# Shared header for the smash deploy scripts. Source this at the top of each script:
#   source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"
#
# Everything is env-overridable so the same scripts run by hand, from the justfile, or from CI:
#   VPS   ssh target of the box            (default root@hafley.codes)
#   PROJ  repo root                        (default: derived from this script's location)
#   BIN   built musl relay binary          (default: the release target under PROJ)
set -euo pipefail

VPS="${VPS:-root@hafley.codes}"
PROJ="${PROJ:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN="${BIN:-$PROJ/signaling/target/x86_64-unknown-linux-musl/release/smash-signaling}"

# Bright prefix so a multi-step run is skimmable in the terminal.
say() { printf '\033[36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31mFATAL:\033[0m %s\n' "$*" >&2; exit 1; }
