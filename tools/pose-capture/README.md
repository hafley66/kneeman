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

## Into the game

```
unzip chris_capture.zip
cp -r assets/friends  <godot-project>/assets/     # -> res://assets/friends/chris/chris_*.png
```

Then register `character.json` through the same JSON path `roster.rs` already parses
(`sheet:"poses"`, `<prefix>_<file>.png`). idle-only is enough to load; fill the rest later.

## Tips for clean cutouts

- Even, frontal light; avoid a background the same color as clothing.
- Segmentation is per-frame ML, not a green screen — a plain wall still helps the edges.
- `cut out` toggle off = raw frames (matte them later with `rembg` if you prefer).
- Shoot every clip from the **same distance + framing** so poses line up in-game.
