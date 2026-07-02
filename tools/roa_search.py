#!/usr/bin/env python3
"""Search the Rivals of Aether Steam Workshop for character packs.

  python3 tools/roa_search.py "captain falcon"
  python3 tools/roa_search.py zelda --pages 2

Prints id, subscriber count, size, tags, title, ranked by subscribers.
Feed the id to steam (subscribe in client, or `steamcmd +workshop_download_item 383980 <id>`),
then point a [[pack]] path in tools/packs.toml at the mod's sprites/ dir.
Stdlib only.
"""
import argparse
import html
import json
import re
import urllib.parse
import urllib.request

APPID = 383980
UA = {"User-Agent": "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/126 Safari/537.36"}


def search_ids(query: str, pages: int) -> dict[str, str]:
    """id -> title from the workshop browse pages (first occurrence order)."""
    out: dict[str, str] = {}
    for p in range(1, pages + 1):
        q = urllib.parse.urlencode({"appid": APPID, "searchtext": query,
                                    "browsesort": "textsearch", "p": p})
        req = urllib.request.Request(f"https://steamcommunity.com/workshop/browse/?{q}", headers=UA)
        page = urllib.request.urlopen(req).read().decode("utf-8", "replace")
        for m in re.finditer(
                r'<a href="https://steamcommunity\.com/sharedfiles/filedetails/\?id=(\d+)">([^<]+)</a>',
                page):
            out.setdefault(m.group(1), html.unescape(m.group(2)))
    return out


def details(ids: list[str]) -> list[dict]:
    body = {"itemcount": len(ids)}
    for i, fid in enumerate(ids):
        body[f"publishedfileids[{i}]"] = fid
    req = urllib.request.Request(
        "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
        data=urllib.parse.urlencode(body).encode(), headers=UA)
    resp = json.load(urllib.request.urlopen(req))
    return resp["response"]["publishedfiledetails"]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("query")
    ap.add_argument("--pages", type=int, default=1, help="browse pages to scan (30/page) [1]")
    ap.add_argument("--all-tags", action="store_true", help="include non-Characters items")
    args = ap.parse_args()

    found = search_ids(args.query, args.pages)
    if not found:
        raise SystemExit("no results")
    rows = []
    for f in details(list(found)):
        if str(f.get("result")) != "1":
            continue
        tags = [t["tag"] for t in f.get("tags", [])]
        if not args.all_tags and "Characters" not in tags:
            continue
        rows.append((int(f.get("lifetime_subscriptions", 0)),
                     f["publishedfileid"],
                     int(f.get("file_size", 0)),
                     ",".join(tags),
                     f.get("title", found.get(f["publishedfileid"], "?"))))
    rows.sort(reverse=True)
    for subs, fid, size, tags, title in rows:
        print(f"{fid}  subs={subs:>7}  {size/1e6:5.1f}MB  [{tags}]  {title}")


if __name__ == "__main__":
    main()
