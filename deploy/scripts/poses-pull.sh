#!/usr/bin/env bash
# Pull captured pose zips from the box down to a local folder, and (optionally) unzip each new one.
# The counterpart to poses.sh: friend captures on the phone -> PUT to the box -> this rsyncs it here.
#
# All env-overridable (defaults in [brackets]):
#   VPS               ssh target              [root@hafley.codes]
#   POSES_UPLOAD_DIR  remote upload sink      [/var/www/smash-poses-uploads]
#   POSES_LOCAL       local landing folder    [$PROJ/captures]
#   POSES_UNZIP       1 = unzip each new zip  [1]
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

POSES_UPLOAD_DIR="${POSES_UPLOAD_DIR:-/var/www/smash-poses-uploads}"
POSES_LOCAL="${POSES_LOCAL:-$PROJ/captures}"
POSES_UNZIP="${POSES_UNZIP:-1}"

mkdir -p "$POSES_LOCAL"
say "pulling $VPS:$POSES_UPLOAD_DIR/ -> $POSES_LOCAL/"
# only the zips; skip the WebDAV temp dir
rsync -az --prune-empty-dirs \
  --include='*/' --include='*.zip' --exclude='*' \
  "$VPS:$POSES_UPLOAD_DIR/" "$POSES_LOCAL/"

if [ "$POSES_UNZIP" = 1 ]; then
  shopt -s nullglob
  for z in "$POSES_LOCAL"/*.zip; do
    d="${z%.zip}"
    [ -d "$d" ] && continue          # already extracted
    say "unzip $(basename "$z") -> $(basename "$d")/"
    unzip -q -o "$z" -d "$d"
  done
fi
say "done: $(ls -1 "$POSES_LOCAL"/*.zip 2>/dev/null | wc -l | tr -d ' ') zip(s) in $POSES_LOCAL"
