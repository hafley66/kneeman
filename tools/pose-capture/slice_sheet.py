#!/usr/bin/env python3
"""Slice a spritesheet into individual transparent PNGs.

Auto mode (default): detect the background color from the sheet's border pixels,
label connected non-background regions, crop each to its bounding box, and write
them with the background made transparent. Handles the irregular packing of
ripped sheets (spriters-resource style) without knowing a grid.

Grid mode: --grid COLSxROWS cuts uniform cells, trims each to content, and
transparentizes the background.

Usage:
  python3 slice_sheet.py sheet.png -o frames/
  python3 slice_sheet.py sheet.png -o frames/ --grid 8x4
  python3 slice_sheet.py sheet.png -o frames/ --bg ff00ff --tol 48 --min-area 300
  python3 slice_sheet.py sheet.png --list          # print boxes, write nothing

Needs Pillow (pip install pillow).
"""
import argparse
import collections
import os
import sys

from PIL import Image


def parse_args():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("sheet", help="spritesheet image")
    p.add_argument("-o", "--out", default="frames", help="output dir [frames]")
    p.add_argument("--grid", metavar="CxR", help="uniform grid COLSxROWS instead of auto detect")
    p.add_argument("--bg", metavar="RRGGBB", help="background color hex (default: sampled from border)")
    p.add_argument("--tol", type=int, default=32, help="color distance from bg to count as sprite [32]")
    p.add_argument("--min-area", type=int, default=64, help="drop components smaller than this many px [64]")
    p.add_argument("--max-w", type=int, default=0, help="drop components wider than this before merging (0=off) — kills renders/logos on mixed sheets")
    p.add_argument("--max-h", type=int, default=0, help="drop components taller than this before merging (0=off)")
    p.add_argument("--merge", type=int, default=6, help="merge boxes closer than this many px (detached limbs/fx) [6]")
    p.add_argument("--pad", type=int, default=1, help="padding around each crop [1]")
    p.add_argument("--prefix", default="frame", help="output filename prefix [frame]")
    p.add_argument("--list", action="store_true", help="print bounding boxes, write nothing")
    return p.parse_args()


def sample_bg(img):
    """Most common color among border pixels."""
    w, h = img.size
    px = img.load()
    counts = collections.Counter()
    for x in range(w):
        counts[px[x, 0]] += 1
        counts[px[x, h - 1]] += 1
    for y in range(h):
        counts[px[0, y]] += 1
        counts[px[w - 1, y]] += 1
    return counts.most_common(1)[0][0]


def build_mask(img, bg, tol):
    """One bool per pixel: is this a sprite pixel. If the sheet has real
    transparency, alpha is the mask; otherwise distance from bg color."""
    w, h = img.size
    data = list(img.getdata())
    has_alpha = any(p[3] < 255 for p in data[:: max(1, len(data) // 5000)])
    if has_alpha:
        return [p[3] > 8 for p in data], w, h
    br, bgc, bb = bg[0], bg[1], bg[2]
    t2 = tol * tol
    mask = [
        (p[0] - br) ** 2 + (p[1] - bgc) ** 2 + (p[2] - bb) ** 2 > t2
        for p in data
    ]
    return mask, w, h


def label_boxes(mask, w, h):
    """Scanline run labeling with union-find; returns bounding boxes per component."""
    parent = {}

    def find(a):
        while parent[a] != a:
            parent[a] = parent[parent[a]]
            a = parent[a]
        return a

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    next_label = 0
    prev_runs = []  # (x0, x1, label) for the previous row
    boxes = {}      # label -> [x0, y0, x1, y1]
    for y in range(h):
        row = mask[y * w:(y + 1) * w]
        runs = []
        x = 0
        while x < w:
            if row[x]:
                x0 = x
                while x < w and row[x]:
                    x += 1
                runs.append([x0, x - 1, None])
            else:
                x += 1
        for run in runs:
            for p0, p1, plab in prev_runs:
                if run[0] <= p1 and run[1] >= p0:  # 4-connected overlap
                    if run[2] is None:
                        run[2] = find(plab)
                    else:
                        union(run[2], plab)
            if run[2] is None:
                run[2] = next_label
                parent[next_label] = next_label
                next_label += 1
        for x0, x1, lab in runs:
            r = find(lab)
            b = boxes.setdefault(r, [x0, y, x1, y])
            b[0] = min(b[0], x0)
            b[1] = min(b[1], y)
            b[2] = max(b[2], x1)
            b[3] = max(b[3], y)
        prev_runs = runs
    # collapse boxes whose labels merged after they were seeded
    merged = {}
    for lab, b in boxes.items():
        r = find(lab)
        m = merged.setdefault(r, list(b))
        m[0] = min(m[0], b[0])
        m[1] = min(m[1], b[1])
        m[2] = max(m[2], b[2])
        m[3] = max(m[3], b[3])
    return list(merged.values())


def merge_near(boxes, dist):
    """Union boxes whose rects, grown by dist, intersect. Repeats to fixpoint."""
    changed = True
    while changed:
        changed = False
        out = []
        while boxes:
            a = boxes.pop()
            i = 0
            while i < len(boxes):
                b = boxes[i]
                if (a[0] - dist <= b[2] and a[2] + dist >= b[0]
                        and a[1] - dist <= b[3] and a[3] + dist >= b[1]):
                    a = [min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3])]
                    boxes.pop(i)
                    changed = True
                else:
                    i += 1
            out.append(a)
        boxes = out
    return boxes


def sort_reading_order(boxes):
    """Row-major: bucket by vertical overlap, then left-to-right."""
    rows = []
    for b in sorted(boxes, key=lambda b: b[1]):
        for row in rows:
            if b[1] <= row["y1"] and b[3] >= row["y0"]:
                row["boxes"].append(b)
                row["y0"] = min(row["y0"], b[1])
                row["y1"] = max(row["y1"], b[3])
                break
        else:
            rows.append({"y0": b[1], "y1": b[3], "boxes": [b]})
    out = []
    for row in sorted(rows, key=lambda r: r["y0"]):
        out.extend(sorted(row["boxes"], key=lambda b: b[0]))
    return out


def transparentize(crop, bg, tol):
    if crop.mode != "RGBA":
        crop = crop.convert("RGBA")
    data = crop.getdata()
    t2 = tol * tol
    br, bgc, bb = bg[0], bg[1], bg[2]
    crop.putdata([
        (p[0], p[1], p[2], 0)
        if (p[0] - br) ** 2 + (p[1] - bgc) ** 2 + (p[2] - bb) ** 2 <= t2
        else p
        for p in data
    ])
    return crop


def main():
    args = parse_args()
    img = Image.open(args.sheet).convert("RGBA")
    w, h = img.size
    bg = tuple(int(args.bg[i:i + 2], 16) for i in (0, 2, 4)) + (255,) if args.bg else sample_bg(img)
    print(f"sheet {w}x{h}  bg rgba{bg}")

    if args.grid:
        cols, rows = (int(v) for v in args.grid.lower().split("x"))
        cw, ch = w // cols, h // rows
        boxes = []
        for r in range(rows):
            for c in range(cols):
                cell = img.crop((c * cw, r * ch, (c + 1) * cw, (r + 1) * ch))
                cell = transparentize(cell, bg, args.tol)
                bbox = cell.getbbox()
                if not bbox or (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]) < args.min_area:
                    continue
                boxes.append((c * cw + bbox[0], r * ch + bbox[1], c * cw + bbox[2] - 1, r * ch + bbox[3] - 1))
    else:
        mask, w, h = build_mask(img, bg, args.tol)
        boxes = label_boxes(mask, w, h)
        boxes = [b for b in boxes if (b[2] - b[0] + 1) * (b[3] - b[1] + 1) >= args.min_area]
        if args.max_w:
            boxes = [b for b in boxes if b[2] - b[0] + 1 <= args.max_w]
        if args.max_h:
            boxes = [b for b in boxes if b[3] - b[1] + 1 <= args.max_h]
        boxes = merge_near(boxes, args.merge)

    boxes = sort_reading_order(boxes)
    print(f"{len(boxes)} frames")
    if args.list:
        for i, b in enumerate(boxes):
            print(f"  {args.prefix}_{i:03d}  x={b[0]} y={b[1]} w={b[2]-b[0]+1} h={b[3]-b[1]+1}")
        return

    os.makedirs(args.out, exist_ok=True)
    pad = args.pad
    for i, b in enumerate(boxes):
        crop = img.crop((max(0, b[0] - pad), max(0, b[1] - pad),
                         min(w, b[2] + 1 + pad), min(h, b[3] + 1 + pad)))
        crop = transparentize(crop, bg, args.tol)
        path = os.path.join(args.out, f"{args.prefix}_{i:03d}.png")
        crop.save(path)
    print(f"wrote {len(boxes)} PNGs to {args.out}/")


if __name__ == "__main__":
    sys.exit(main())
