#!/usr/bin/env bash
# Ship the pose-capture app + its nginx snippet to the box, idempotently wire the include, create the
# WebDAV upload dir with www-data perms, then `nginx -t` + reload. Mirrors deploy/scripts/signaling.sh.
# Serves https://hafley.codes/game/poses/ ; the phone PUTs capture zips to /game/poses/uploads/.
#
# All env-overridable (defaults in [brackets]):
#   VPS               ssh target                    [root@hafley.codes]
#   POSES_SRC         local app dir to ship         [$PROJ/tools/pose-capture]
#   POSES_APP_DIR     remote app root               [/var/www/smash-poses]
#   POSES_UPLOAD_DIR  remote upload sink            [/var/www/smash-poses-uploads]
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

POSES_SRC="${POSES_SRC:-$PROJ/tools/pose-capture}"
POSES_APP_DIR="${POSES_APP_DIR:-/var/www/smash-poses}"
POSES_UPLOAD_DIR="${POSES_UPLOAD_DIR:-/var/www/smash-poses-uploads}"

[ -f "$POSES_SRC/index.html" ] || die "app not found at $POSES_SRC/index.html"

say "shipping nginx snippet + app to $VPS"
scp "$PROJ/deploy/nginx/poses.conf" "$VPS:/etc/nginx/snippets/poses.conf"
ssh "$VPS" "mkdir -p '$POSES_APP_DIR' '$POSES_UPLOAD_DIR/.tmp'"
rsync -az --delete "$POSES_SRC/" "$VPS:$POSES_APP_DIR/"

say "wiring include + upload perms + reload"
ssh "$VPS" "UP='$POSES_UPLOAD_DIR' APP='$POSES_APP_DIR' bash -s" <<'REMOTE'
set -euo pipefail
SITE=/etc/nginx/sites-enabled/default
# Insert `include snippets/poses.conf;` right after the godot include, once.
if ! grep -q "snippets/poses.conf" "$SITE"; then
  # backup OUTSIDE sites-enabled/ -- nginx globs sites-enabled/* and a *.bak there is a
  # duplicate default_server that fails `nginx -t`.
  mkdir -p /root/nginx-backups
  cp "$SITE" "/root/nginx-backups/sites-default.$(date +%s).bak"
  sed -i "/include snippets\/godot.conf;/a\\    include snippets/poses.conf;" "$SITE"
fi
# nginx (www-data) must own the upload sink so WebDAV PUT can write.
chown -R www-data:www-data "$UP"
chmod 0775 "$UP" "$UP/.tmp"
nginx -t
systemctl reload nginx
echo "reloaded. app=$APP  uploads=$UP"
REMOTE
say "live: https://hafley.codes/game/poses/"
