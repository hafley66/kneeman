# deploy/nginx — the box's live nginx config, tracked

Box: `root@hafley.codes` (Vultr, Ubuntu 24.04, nginx 1.24, `--with-http_dav_module`).
Pulled with `rsync -az root@hafley.codes:/etc/nginx/ …`. TLS is certbot-managed (certs live in
`/etc/letsencrypt/`, **not** in this repo).

## live path → repo file

| live | repo | notes |
|---|---|---|
| `/etc/nginx/nginx.conf` | `nginx.conf` | http{} core. Has `client_max_body_size 20M` + a **dead** stray `server{listen 443;…upload…}` block |
| `/etc/nginx/sites-enabled/default` | `sites-default.conf` | the served vhost; includes the 6 snippets below after the godot include |
| `/etc/nginx/snippets/rtc.conf` | `rtc.conf` | wss `/rtc` relay |
| `/etc/nginx/snippets/ev.conf` | `ev.conf` | POST `/ev` firehose |
| `/etc/nginx/snippets/turn.conf` | `turn.conf` | GET `/turn` cred mint |
| `/etc/nginx/snippets/godot.conf` | `godot.conf` | `/game/` → `/var/www/smash-godot/` (the web export) |
| `/etc/nginx/snippets/web.conf` | `web.conf` | static web bits |
| `/etc/nginx/snippets/matchbox-ws.conf` | `matchbox-ws.conf` | ws → `matchbox_server` on `127.0.0.1:3536` |
| `/etc/nginx/snippets/poses.conf` | `poses.conf` | `/game/poses/` capture app + WebDAV upload sink |

## dead cruft on the box (captured, not wired by us)

- `sites-default.conf`: `location /upload_file { proxy_pass http://localhost:8080; }` — **nothing listens on 8080**.
- `nginx.conf`: a second `server { listen 443; … /upload_file … }` inside `http{}`, no TLS/cert — inert.

Both are old upload experiments. Left in place (only captured here) so a later cleanup is a reviewable diff.

## redeploy

`just poses-deploy` ships `poses.conf` + the app and wires the include (idempotent, `nginx -t` gated).
`just vps-deploy` ships the matchbox/signaling snippets. Neither writes certs.
