#!/usr/bin/env python3
"""Regenerate ref/ ghost images from an imported character's strips.

  python3 tools/pose-capture/make_refs.py            # default: assets/falcon
  python3 tools/pose-capture/make_refs.py assets/zetterburn

For each CLIPS entry in index.html (name + frame count) it finds
`assets/<char>/<name>_strip<N>.png`, samples <frames> interior frames from the
strip, and writes `ref/<name>.png` or `ref/<name>1.png, <name>2.png ...` — the
exact stems updateGhost() loads. Clips with no matching strip (RoA has no
hang/climb) are left alone, so hand-picked refs survive a re-run.
"""
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
REF = HERE / "ref"

# name -> frames, mirrors CLIPS in index.html.
CLIPS = {
    "idle": 2, "walk": 2, "run": 2, "skid": 1, "crouch": 1, "jump": 1,
    "fall": 1, "hang": 1, "climb": 2, "jab": 2, "nair": 2, "dtilt": 2,
    "dair": 2, "wallbounce": 1,
}


def sample(n_frames: int, want: int) -> list[int]:
    """`want` interior frame indices from a strip of `n_frames` (skews past wind-up)."""
    if n_frames <= want:
        return list(range(n_frames))
    return [round((i + 1) * n_frames / (want + 1)) for i in range(want)]


def main() -> None:
    try:
        from PIL import Image
    except ImportError:
        py = ROOT / "tools" / ".venv" / "bin" / "python3"
        raise SystemExit(f"[refs] Pillow missing; run with {py}")
    char_dir = ROOT / (sys.argv[1] if len(sys.argv) > 1 else "assets/falcon")
    strips = {m.group(1): (p, int(m.group(2)))
              for p in sorted(char_dir.glob("*_strip*.png"))
              if (m := re.match(r"(.+)_strip(\d+)$", p.stem))}
    REF.mkdir(exist_ok=True)
    for name, want in CLIPS.items():
        if name not in strips:
            print(f"[refs] {name}: no strip in {char_dir.name}; keeping existing ref")
            continue
        path, n = strips[name]
        with Image.open(path) as im:
            fw = im.width // n
            for out_i, frame_i in enumerate(sample(n, want)):
                stem = f"{name}{out_i + 1}" if want > 1 else name
                cell = im.convert("RGBA").crop((frame_i * fw, 0, (frame_i + 1) * fw, im.height))
                cell.save(REF / f"{stem}.png")
                print(f"[refs] {stem}.png <- {path.name} frame {frame_i + 1}/{n}")


if __name__ == "__main__":
    main()
