# pose-capture — film a friend into a playable fighter

A single static page. Camera → **live person cutout** (MediaPipe Selfie Segmentation)
→ guided pose-by-pose capture → exports a zip of transparent PNGs + `character.json`
that `roster::parse_character` loads. No backend, no build.

## Run (laptop webcam — friend stands in front)

```sh
cd tools/pose-capture
python3 -m http.server 8000
# open http://localhost:8000   (localhost is a secure context, so the camera works)
```

## Run (phone as the camera)

`getUserMedia` needs HTTPS off-localhost. Tunnel it:

```sh
python3 -m http.server 8000 &
npx cloudflared tunnel --url http://localhost:8000   # or: ngrok http 8000
# open the https URL it prints on the phone, allow camera
```

## Flow

1. Set **dir** (`friends/chris`) and **prefix** (`chris`) up top.
2. Left rail lists the 14 clips (mirrors `ALL_CLIPS` in `sprite.rs`). `idle` is required;
   every other clip falls back down `clip_fallback()` to idle, so a partial capture still plays.
3. Pick a clip, friend hits the pose, press **Capture frame (3s)** (countdown) or **Snap now**.
   Multi-frame clips (walk, jab, dair…) ask for each frame in turn.
4. **Export .zip**.

Captures persist in the browser (IndexedDB, keyed by prefix): close the tab, come
back next week, the shots are still there — key just the new clips and re-export.
**Clear clip** deletes that clip's saved frames; switching prefix switches profiles.

## Into the game

```
unzip chris_capture.zip
cp -r assets/friends  <godot-project>/assets/     # -> res://assets/friends/chris/chris_*.png
```

Then register `character.json` through the same JSON path `roster.rs` already parses
(`sheet:"poses"`, `<prefix>_<file>.png`). idle-only is enough to load; fill the rest later.

## Reference ghosts from a spritesheet

Drop PNGs in `ref/` named after the frame stems the exporter writes
(`idle1.png idle2.png`, `walk1.png walk2.png`, `skid.png`, …) and the tool overlays
the matching one at 45% opacity on the camera — the friend poses to line up with it.
Missing files just skip the ghost; the `ref` checkbox in the header toggles it.

From an imported RoA character (after `roa_get.py` + `fetch_packs.py`):

```sh
python3 tools/pose-capture/make_refs.py                 # assets/falcon (default)
python3 tools/pose-capture/make_refs.py assets/other    # any strip character
```

samples the right number of frames per clip straight from the `_stripN` files.
Clips the character lacks (RoA has no hang/climb) keep their existing refs.

To cut a ripped sheet (spriters-resource style, irregular packing) into frames:

```sh
python3 slice_sheet.py falcon_sheet.png -o /tmp/frames --list   # preview boxes
python3 slice_sheet.py falcon_sheet.png -o /tmp/frames          # write crops
# eyeball /tmp/frames, then copy + rename the keepers:
cp /tmp/frames/frame_004.png ref/idle1.png
```

Auto mode samples the background color from the sheet border and cuts connected
regions (`--bg ff00ff --tol --min-area --merge` to tune); `--grid 8x4` for uniform
sheets. Needs Pillow. Redeploy with `just poses-deploy` so the phone sees `ref/`.

## Tips for clean cutouts

- Even, frontal light; avoid a background the same color as clothing.
- Segmentation is per-frame ML, not a green screen — a plain wall still helps the edges.
- `cut out` toggle off = raw frames (matte them later with `rembg` if you prefer).
- Shoot every clip from the **same distance + framing** so poses line up in-game.
