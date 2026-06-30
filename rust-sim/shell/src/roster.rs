//! Character roster: the built-in fighters, the `assets/roster.json` loader, and the dict->Character
//! parsing. Pure data + asset IO, lifted out of `kneeman` (the Godot node) so the file holding the
//! node is just the node.
//!
//! Cosmetic only -- a character's art is never folded into `smash_net::checksum`, so adding or
//! reordering the roster cannot desync a netplay session. Each fighter slot holds an index into the
//! roster; char-select just writes those indices. The list is two built-ins (frog/zombie) followed
//! by whatever `assets/roster.json` declares (written by `tools/fetch_packs.py` -> `just packs`).

use godot::classes::{FileAccess, Json};
use godot::prelude::*;

pub(crate) struct Character {
    pub(crate) dir: String,      // asset subdir under res://assets/
    pub(crate) scale: f32,       // node scale so the art lands ~140px tall (near the ECB height)
    pub(crate) offset_y: f32,    // sprite offset (texture px) so the feet sit on pos
    pub(crate) sheet: Sheet,     // how this character's PNGs are laid out on disk
    pub(crate) clips: Vec<Clip>, // one per CharState clip name (see clip_for)
}

/// How a character's frames are stored.
pub(crate) enum Sheet {
    /// One horizontal strip per clip, sliced into `frames` cells. `frame_px` is the cell width; 0
    /// means "derive from texture width / frame count" (the fetch script leaves it 0). Cell height
    /// is the full strip height, so non-square Rivals frames slice correctly. File = `<file>.png`.
    Strip { frame_px: f32 },
    /// One whole PNG per pose, named `<prefix>_<file>.png`. Each entry in `clip.files` is one frame.
    Poses { prefix: String },
}

/// One animation clip. For Strip, `files` holds the single strip name and `frames` is the cell
/// count; for Poses, `files` is the per-frame pose list and `frames` is ignored.
pub(crate) struct Clip {
    pub(crate) name: String,
    pub(crate) files: Vec<String>,
    pub(crate) frames: i32,
    pub(crate) fps: f64,
    pub(crate) looped: bool,
}

pub(crate) fn clip(name: &str, files: &[&str], frames: i32, fps: f64, looped: bool) -> Clip {
    Clip {
        name: name.to_string(),
        files: files.iter().map(|s| s.to_string()).collect(),
        frames,
        fps,
        looped,
    }
}

/// Display names for the roster, in index order (the menu char-select labels each pick with these).
pub(crate) fn roster_names() -> Vec<String> {
    roster().into_iter().map(|c| c.dir).collect()
}

/// The live roster: the two built-ins, then any characters declared in `assets/roster.json`.
pub(crate) fn roster() -> Vec<Character> {
    let mut v = vec![frog(), zombie()];
    v.extend(load_roster_json());
    v
}

/// P1 default: the Kenney/PixelFrog ninja frog (32px strips). CC0 placeholder art.
pub(crate) fn frog() -> Character {
    Character {
        dir: "pixelfrog/ninjafrog".to_string(),
        scale: 4.4, // 32px art -> ~140px tall, matching the ECB body
        offset_y: -12.0,
        sheet: Sheet::Strip { frame_px: 32.0 },
        clips: vec![
            clip("idle", &["idle"], 11, 14.0, true),
            clip("walk", &["run"], 12, 14.0, true),
            clip("run", &["run"], 12, 20.0, true),
            clip("crouch", &["fall"], 1, 1.0, false),
            clip("skid", &["fall"], 1, 1.0, false),
            clip("jump", &["jump"], 1, 1.0, false),
            clip("fall", &["fall"], 1, 1.0, false),
            clip("hang", &["wall_jump"], 5, 12.0, true),
            clip("climb", &["double_jump"], 6, 14.0, true),
            clip("jab", &["hit"], 7, 20.0, false),
            clip("nair", &["double_jump"], 6, 18.0, true),
            clip("dtilt", &["hit"], 7, 26.0, false), // pothole swing reuses the punch sheet, one-shot
            clip("dair", &["hit"], 7, 26.0, false), // the stomp: reuse the swing sheet (per-box scrub replays it)
            clip("wallbounce", &["fall"], 1, 1.0, false), // wall hit: a single frozen frame, tilted in render
        ],
    }
}

/// P2 default: the Kenney zombie (single-pose PNGs). Different silhouette from the frog.
pub(crate) fn zombie() -> Character {
    Character {
        dir: "kenney/zombie".to_string(),
        scale: 1.27, // 110px art -> ~140px tall, matching the ECB body
        offset_y: -55.0,
        sheet: Sheet::Poses { prefix: "zombie".to_string() },
        clips: vec![
            clip("idle", &["idle"], 1, 1.0, false),
            clip("walk", &["walk1", "walk2"], 2, 8.0, true),
            clip("run", &["walk1", "walk2"], 2, 13.0, true),
            clip("skid", &["skid"], 1, 1.0, false),
            clip("crouch", &["duck"], 1, 1.0, false),
            clip("jump", &["jump"], 1, 1.0, false),
            clip("fall", &["fall"], 1, 1.0, false),
            clip("hang", &["hang"], 1, 1.0, false),
            clip("climb", &["climb1", "climb2"], 2, 8.0, true),
            clip("jab", &["action1"], 1, 1.0, false),
            clip("nair", &["kick"], 1, 1.0, false),
            clip("dtilt", &["duck"], 1, 1.0, false),     // pothole reuses the duck pose
            clip("wallbounce", &["hurt"], 1, 1.0, false), // wall hit reuses the hurt pose
        ],
    }
}

/// Parse `res://assets/roster.json` (written by `tools/fetch_packs.py`) into extra characters.
/// Missing file or malformed JSON yields an empty list -- the built-ins always work, so a bad
/// roster never bricks the game. Schema (per character):
/// `{ "dir","scale","offset_y","sheet":"strip"|"poses","prefix"?,"frame_px"?,
///    "clips":[{ "name","files":[..],"frames","fps","loop" }] }`
pub(crate) fn load_roster_json() -> Vec<Character> {
    let path = "res://assets/roster.json";
    if !FileAccess::file_exists(path) {
        return Vec::new();
    }
    let Some(text) = FileAccess::open(path, godot::classes::file_access::ModeFlags::READ)
        .map(|f| f.get_as_text().to_string())
    else {
        return Vec::new();
    };
    let parsed = Json::parse_string(text.as_str());
    let Ok(root) = parsed.try_to::<Dictionary>() else {
        return Vec::new();
    };
    let Some(list) = root.get("characters").and_then(|v| v.try_to::<VariantArray>().ok()) else {
        return Vec::new();
    };
    list.iter_shared()
        .filter_map(|v| v.try_to::<Dictionary>().ok())
        .filter_map(parse_character)
        .collect()
}

/// One character dict -> Character. Returns None on a missing required field so one bad entry is
/// skipped rather than poisoning the whole roster.
pub(crate) fn parse_character(d: Dictionary) -> Option<Character> {
    let dir = jstr(&d, "dir")?;
    let scale = jnum(&d, "scale").unwrap_or(1.0) as f32;
    let offset_y = jnum(&d, "offset_y").unwrap_or(0.0) as f32;
    let sheet = match jstr(&d, "sheet").as_deref() {
        Some("poses") => Sheet::Poses { prefix: jstr(&d, "prefix").unwrap_or_default() },
        _ => Sheet::Strip { frame_px: jnum(&d, "frame_px").unwrap_or(0.0) as f32 },
    };
    let clips = d
        .get("clips")
        .and_then(|v| v.try_to::<VariantArray>().ok())
        .map(|arr| {
            arr.iter_shared()
                .filter_map(|c| c.try_to::<Dictionary>().ok())
                .filter_map(parse_clip)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if clips.is_empty() {
        return None;
    }
    Some(Character { dir, scale, offset_y, sheet, clips })
}

pub(crate) fn parse_clip(d: Dictionary) -> Option<Clip> {
    let name = jstr(&d, "name")?;
    let files = d
        .get("files")
        .and_then(|v| v.try_to::<VariantArray>().ok())
        .map(|arr| arr.iter_shared().filter_map(|f| f.try_to::<GString>().ok().map(|g| g.to_string())).collect())
        .unwrap_or_else(|| vec![name.clone()]);
    Some(Clip {
        name,
        files,
        frames: jnum(&d, "frames").unwrap_or(1.0) as i32,
        fps: jnum(&d, "fps").unwrap_or(12.0),
        looped: d.get("loop").and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false),
    })
}

/// Read a string field from a Json-parsed Dictionary.
pub(crate) fn jstr(d: &Dictionary, key: &str) -> Option<String> {
    d.get(key).and_then(|v| v.try_to::<GString>().ok()).map(|g| g.to_string())
}

/// Read a number field (Json numbers come back as f64).
pub(crate) fn jnum(d: &Dictionary, key: &str) -> Option<f64> {
    d.get(key).and_then(|v| v.try_to::<f64>().ok())
}
