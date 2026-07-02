#!/usr/bin/env python3
"""Fetch + convert sprite packs listed in tools/packs.toml into the game roster.

For each [[pack]] it: downloads (or reads a local path), normalizes the art into
`<clip>_strip<N>.png` files under `assets/<name>/`, and rewrites `assets/roster.json` with one
entry per pack. The game's `roster()` (shell/src/kneeman.rs) reads that JSON at boot and the
characters appear after the built-in frog + zombie. Re-running is idempotent: roster.json is
regenerated from scratch each run from whatever packs.toml currently lists.

Pillow is the only non-stdlib dependency; if it's missing this script bootstraps a venv at
tools/.venv, installs Pillow there, and re-execs itself inside it. So `python3 tools/fetch_packs.py`
just works.
"""

from __future__ import annotations

import io
import json
import os
import shutil
import sys
import tomllib
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
MANIFEST = ROOT / "tools" / "packs.toml"
ROSTER_JSON = ASSETS / "roster.json"
CACHE = ROOT / "tools" / ".cache"

# Target on-screen height (px) for a character's idle frame; scale is derived to hit this.
TARGET_H = 140.0

# Source clip name -> our CharState clip name (see clip_for in kneeman.rs). Anything not listed
# passes through unchanged, so extra source clips are still available if the sim ever asks for them.
ALIAS = {
    "dash": "run", "walkturn": "skid", "dashstop": "skid", "land": "fall", "jumpstart": "jump",
    "doublejump": "jump", "airjump": "jump", "djump": "jump",
    "crouchidle": "crouch", "duck": "crouch",
    "attack": "jab", "jab1": "jab", "nattack": "jab", "ftilt": "jab",
    "nair_": "nair", "airattack": "nair",
    "ladder": "climb", "wall": "hang",
    "hurt": "wallbounce",
}

# Non-animation sprites in Rivals workshop dumps: hitstun overlay variants, collision masks,
# projectiles, portraits. Skip any clip whose source name ends with / equals one of these.
SKIP_SUFFIXES = ("_hurt", "hurtbox", "_proj")
SKIP_NAMES = {"portrait", "charselect", "icon", "hud", "result_small", "offscreen", "usa", "plat"}

# Default playback fps + loop flag by (resolved) clip name. packs.toml [pack.fps] overrides fps.
CLIP_DEFAULTS = {
    "idle": (10.0, True), "walk": (12.0, True), "run": (16.0, True),
    "crouch": (1.0, False), "skid": (1.0, False), "jump": (1.0, False),
    "fall": (1.0, False), "hang": (10.0, True), "climb": (10.0, True),
    "jab": (18.0, False), "nair": (16.0, True),
}


def ensure_pillow() -> None:
    try:
        import PIL  # noqa: F401
        return
    except ImportError:
        pass
    venv = ROOT / "tools" / ".venv"
    py = venv / "bin" / "python3"
    if not py.exists():
        print("[packs] Pillow missing -> bootstrapping venv at tools/.venv ...")
        import venv as _venv
        _venv.create(venv, with_pip=True)
        import subprocess
        subprocess.check_call([str(py), "-m", "pip", "install", "--quiet", "--upgrade", "pip"])
        subprocess.check_call([str(py), "-m", "pip", "install", "--quiet", "Pillow"])
    if Path(sys.executable).resolve() != py.resolve():
        os.execv(str(py), [str(py), str(Path(__file__).resolve()), *sys.argv[1:]])


def fetch_bytes(url: str) -> bytes:
    CACHE.mkdir(parents=True, exist_ok=True)
    key = CACHE / (str(abs(hash(url))) + Path(url).suffix)
    if key.exists():
        return key.read_bytes()
    print(f"[packs] downloading {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "smash-fetch-packs"})
    with urllib.request.urlopen(req) as r:
        data = r.read()
    key.write_bytes(data)
    return data


def source_files(pack: dict) -> dict[str, bytes]:
    """Return {filename: bytes} for a pack's inputs, from a local path or a url (png or zip)."""
    out: dict[str, bytes] = {}
    if "path" in pack:
        p = Path(pack["path"]).expanduser()
        if p.is_dir():
            for f in sorted(p.iterdir()):
                if f.suffix.lower() == ".png":
                    out[f.name] = f.read_bytes()
        elif p.suffix.lower() == ".png":
            out[p.name] = p.read_bytes()
        else:
            raise SystemExit(f"[packs] {pack['name']}: path is neither a dir nor a .png: {p}")
        return out
    url = pack["url"]
    data = fetch_bytes(url)
    if url.lower().endswith(".zip") or data[:2] == b"PK":
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            for info in z.infolist():
                if info.filename.lower().endswith(".png"):
                    out[Path(info.filename).name] = z.read(info)
        return out
    out[Path(url).name or "sheet.png"] = data
    return out


def parse_strip_name(stem: str) -> tuple[str, int]:
    """`idle_strip8` -> ("idle", 8); `idle` -> ("idle", 1)."""
    if "_strip" in stem:
        base, _, n = stem.rpartition("_strip")
        digits = "".join(c for c in n if c.isdigit())
        return base, int(digits) if digits else 1
    return stem, 1


def resolve_clip(name: str) -> str:
    key = name.lower().strip()
    return ALIAS.get(key, key)


def clip_meta(name: str, frames: int, fps_overrides: dict) -> dict:
    fps, looped = CLIP_DEFAULTS.get(name, (12.0 if frames > 1 else 1.0, frames > 1))
    if name in fps_overrides:
        fps = float(fps_overrides[name])
    return {"fps": fps, "loop": looped}


def build_strip_pack(pack: dict, files: dict[str, bytes], outdir: Path) -> dict:
    from PIL import Image
    fps_overrides = pack.get("fps", {})
    clips: dict[str, dict] = {}  # resolved name -> {file, frames}
    idle_h = None
    for fname, data in files.items():
        stem = Path(fname).stem
        src_name, frames = parse_strip_name(stem)
        low = src_name.lower().strip()
        if low in SKIP_NAMES or low.endswith(SKIP_SUFFIXES):
            continue
        name = resolve_clip(src_name)
        if name in clips:
            print(f"[packs] {pack['name']}: {src_name} -> {name} collides with {clips[name]['file']}; keeping first")
            continue
        outfile = f"{name}_strip{frames}.png"
        with Image.open(io.BytesIO(data)) as im:
            h = im.height
            if frames > 1 and im.width % frames:
                # GM strips are sometimes exported with stray padding; the renderer slices
                # width/frames, so trim to an exact multiple or every frame drifts.
                even_w = (im.width // frames) * frames
                print(f"[packs] {pack['name']}: {stem} width {im.width} not /{frames}; cropping to {even_w}")
                im.convert("RGBA").crop((0, 0, even_w, h)).save(outdir / outfile)
            else:
                (outdir / outfile).write_bytes(data)
        if name == "idle" or idle_h is None:
            idle_h = h
        clips[name] = {"file": Path(outfile).stem, "frames": frames}
    return finish_character(pack, clips, idle_h or TARGET_H)


def build_grid_pack(pack: dict, files: dict[str, bytes], outdir: Path) -> dict:
    from PIL import Image
    cols = int(pack["cols"])
    rows = int(pack["rows"])
    order = pack["rows_order"]
    if len(order) != rows:
        raise SystemExit(f"[packs] {pack['name']}: rows_order has {len(order)} names, rows={rows}")
    sheet_bytes = next(iter(files.values()))
    sheet = Image.open(io.BytesIO(sheet_bytes)).convert("RGBA")
    cw, ch = sheet.width // cols, sheet.height // rows
    clips: dict[str, dict] = {}
    for r, src_name in enumerate(order):
        name = resolve_clip(src_name)
        # Reflow this row into a single horizontal strip (already a row, so it's a straight crop).
        strip = sheet.crop((0, r * ch, cols * cw, (r + 1) * ch))
        outfile = f"{name}_strip{cols}.png"
        strip.save(outdir / outfile)
        clips[name] = {"file": Path(outfile).stem, "frames": cols}
    return finish_character(pack, clips, ch)


def finish_character(pack: dict, clips: dict[str, dict], idle_h: float) -> dict:
    fps_overrides = pack.get("fps", {})
    name_dir = pack["name"]
    scale = float(pack["scale"]) if "scale" in pack else round(TARGET_H / max(idle_h, 1.0), 4)
    # Feet on the position: lift the centered sprite by half the cell height (in cell px).
    offset_y = float(pack["offset_y"]) if "offset_y" in pack else round(-idle_h / 2.0, 1)
    clip_list = []
    for name, info in clips.items():
        meta = clip_meta(name, info["frames"], fps_overrides)
        clip_list.append({
            "name": name,
            "files": [info["file"]],
            "frames": info["frames"],
            "fps": meta["fps"],
            "loop": meta["loop"],
        })
    if not any(c["name"] == "idle" for c in clip_list) and clip_list:
        clip_list[0]["name"] = "idle"  # guarantee an idle so the sprite has something to play
    return {
        "dir": name_dir,
        "scale": scale,
        "offset_y": offset_y,
        "sheet": "strip",
        "frame_px": 0,  # auto: texture width / frame count
        "clips": clip_list,
    }


def main() -> None:
    ensure_pillow()
    if not MANIFEST.exists():
        raise SystemExit(f"[packs] no manifest at {MANIFEST}")
    spec = tomllib.loads(MANIFEST.read_text())
    packs = spec.get("pack", [])
    if not packs:
        print("[packs] packs.toml has no [[pack]] entries; nothing to do.")
        # Still drop an empty roster so the file is well-formed.
        ROSTER_JSON.write_text(json.dumps({"characters": []}, indent=2) + "\n")
        return
    characters = []
    for pack in packs:
        name = pack["name"]
        outdir = ASSETS / name
        if outdir.exists():
            shutil.rmtree(outdir)
        outdir.mkdir(parents=True)
        files = source_files(pack)
        if not files:
            raise SystemExit(f"[packs] {name}: no PNGs found in source")
        kind = pack.get("kind", "rivals_strip")
        if kind == "grid":
            entry = build_grid_pack(pack, files, outdir)
        else:
            entry = build_strip_pack(pack, files, outdir)
        characters.append(entry)
        print(f"[packs] built {name}: {len(entry['clips'])} clips, scale={entry['scale']}")
    ROSTER_JSON.write_text(json.dumps({"characters": characters}, indent=2) + "\n")
    print(f"[packs] wrote {ROSTER_JSON.relative_to(ROOT)} ({len(characters)} characters)")
    print("[packs] now run `just import` (or `just packs` re-runs it) to bundle into the .pck")


if __name__ == "__main__":
    main()
