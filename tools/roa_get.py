#!/usr/bin/env python3
"""Download Rivals of Aether workshop items headlessly.

  python3 tools/roa_get.py 2112527215            # -> tools/.cache/workshop/2112527215/
  python3 tools/roa_get.py 2035128973 2990243359 # several ids, ONE Steam session
  python3 tools/roa_get.py 2112527215 -o /tmp/x

Auth: the refresh token saved by `steam_qr.py login` (scan once, download forever).
Metadata comes from the public GetPublishedFileDetails endpoint (no API key),
content from Steam's CDN via manifest request codes.

Chain: roa_search.py <query> -> roa_get.py <id> -> [[pack]] in packs.toml with
path = tools/.cache/workshop/<id>/sprites -> fetch_packs.py -> roster.json.
"""
import argparse
import json
import pathlib
import sys
import urllib.parse
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from steam_qr import patch_zstd_chunks, token_login  # noqa: E402

APPID = 383980
ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_OUT = ROOT / "tools" / ".cache" / "workshop"


def details(fileid: str) -> dict:
    data = urllib.parse.urlencode(
        {"itemcount": 1, "publishedfileids[0]": fileid}).encode()
    resp = json.load(urllib.request.urlopen(
        "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/", data))
    d = resp["response"]["publishedfiledetails"][0]
    if str(d.get("result")) != "1":
        raise SystemExit(f"[roa_get] workshop item {fileid}: result={d.get('result')}")
    return d


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("fileids", nargs="+", help="workshop item id(s) (from roa_search.py)")
    ap.add_argument("-o", "--out", default=None,
                    help=f"output dir, single id only [{DEFAULT_OUT}/<id>]")
    args = ap.parse_args()
    if args.out and len(args.fileids) > 1:
        raise SystemExit("[roa_get] -o only makes sense with a single id")

    items = [(fid, details(fid)) for fid in args.fileids]

    from steam.client import SteamClient
    from steam.client.cdn import CDNClient
    from steam.enums import EResult

    patch_zstd_chunks()
    c = SteamClient()
    r = token_login(c)
    if r != EResult.OK:
        raise SystemExit(f"[roa_get] CM login failed: {r!r}; rerun `steam_qr.py login`")
    cdn = CDNClient(c)
    for fid, d in items:
        gid = int(d["hcontent_file"])
        outdir = pathlib.Path(args.out) if args.out else DEFAULT_OUT / fid
        print(f"[roa_get] {d['title']!r} ({int(d['file_size'])/1e6:.1f}MB) gid={gid}")
        code = cdn.get_manifest_request_code(APPID, APPID, gid)
        manifest = cdn.get_manifest(APPID, APPID, gid, manifest_request_code=code)
        n = 0
        for f in manifest.iter_files():
            if f.is_directory:
                continue
            dest = outdir / f.filename
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(f.read())
            n += 1
        print(f"[roa_get] wrote {n} files -> {outdir}")
        print(f"[roa_get] packs.toml:  path = \"{outdir / 'sprites'}\"  kind = \"rivals_strip\"")
    # disconnect WITHOUT ClientLogOff: logging off a refresh-token session revokes the token
    # server-side (observed twice: token survives exit-without-logoff, dies right after logout()).
    c.disconnect()
    return 0


if __name__ == "__main__":
    sys.exit(main())
