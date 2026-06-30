//! Sprite + label rendering helpers: tint / impact tells, HUD + edge tags, and the
//! `AnimatedSprite2D` clip/frame machinery (`clip_for`, `SpriteFrames` building, attack-frame
//! sync). Free functions lifted out of the `KneeMan` node.

use godot::classes::{AnimatedSprite2D, AtlasTexture, Label, SpriteFrames, Texture2D};
use godot::prelude::*;
use godot::tools::try_load;

use crate::kneeman::gv;
use crate::roster::{Character, Sheet};
use crate::sim::{self, CharState, Fighter, Tune};

/// The sprite's modulate: hit flash > intangible green > the player's color.
pub(crate) fn sprite_tint(f: &Fighter, color: Color) -> Color {
    if f.hitlag > 0 {
        Color::from_rgb(1.0, 1.0, 1.0) // impact freeze: blow out to white (the hit "pop")
    } else if f.hitstun > 0 {
        Color::from_rgb(1.0, 0.95, 0.55) // hit flash while launched
    } else if f.intangible {
        Color::from_rgb(0.30, 0.95, 0.40)
    } else {
        color
    }
}

/// Impact squash-pop: on a connect both fighters freeze (`hitlag`) and we briefly scale the sprite up,
/// easing back as the freeze decays — the classic platform-fighter "pop". 1.0 (no change) otherwise.
/// Pure function of sim state, so it stays rollback-consistent.
pub(crate) fn impact_pop(f: &Fighter) -> f32 {
    if f.hitlag <= 0 {
        return 1.0;
    }
    1.0 + 0.20 * (f.hitlag as f32 / 8.0).min(1.0)
}

/// A world-space nametag: small, the player's color, with a dark outline so it reads over the
/// light stage. Centered horizontally each frame in `place_tag` (Label origin is top-left).
pub(crate) fn make_tag(name: &str, color: Color, font_px: i32) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.set_text(name);
    l.add_theme_font_size_override("font_size", font_px);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 6);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.05, 0.06, 0.10, 0.92));
    l.set_z_index(100); // above the sprites
    l
}

/// An off-stage chip: screen-pinned, the player's color on a dark rounded chip, hidden until the
/// fighter leaves the view. Text (name + arrow + distance) and position are set in `place_edge_tags`.
pub(crate) fn make_edge_tag(color: Color) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.add_theme_font_size_override("font_size", 26);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 5);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.04, 0.05, 0.09, 0.95));
    let mut bg = godot::classes::StyleBoxFlat::new_gd();
    bg.set_bg_color(Color::from_rgba(0.08, 0.10, 0.16, 0.85));
    bg.set_corner_radius_all(8);
    bg.set_content_margin_all(8.0);
    l.add_theme_stylebox_override("normal", &bg);
    l.set_visible(false);
    l
}

/// A bottom-HUD damage label: big, outlined, on a dark chip so the % reads over the white stage.
/// Color + text are refreshed every frame in `update_hud`.
pub(crate) fn make_hud_label(color: Color) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.add_theme_font_size_override("font_size", 38);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 8);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.04, 0.05, 0.09, 0.95));
    let mut bg = godot::classes::StyleBoxFlat::new_gd();
    bg.set_bg_color(Color::from_rgba(0.07, 0.09, 0.14, 0.80));
    bg.set_corner_radius_all(8);
    bg.set_content_margin_all(10.0);
    l.add_theme_stylebox_override("normal", &bg);
    l.set_z_index(100);
    l
}

/// Position a nametag above a fighter's feet position, centered on the body.
pub(crate) fn place_tag(tag: &mut Gd<Label>, feet: sim::Vector2) {
    const TAG_RISE: f32 = 168.0; // above the feet, clear of the ~140px-tall sprite's head
    let half_w = tag.get_size().x * 0.5;
    let head = gv(feet) + Vector2::new(-half_w, -TAG_RISE);
    tag.set_global_position(head);
}

/// CharState -> SpriteFrames clip. 15 states collapse onto ~9 clips (choppy by design;
/// the Kenney pose set has no per-state art). Air splits rise/fall by vertical velocity.
pub(crate) fn clip_for(f: &Fighter) -> &'static str {
    use CharState::*;
    // A fresh wall bounce overrides the launch state: show the tilted bounce frame while the window
    // (set in the sim's hitstun block) is open. Cosmetic only — the sim drives the physics.
    if f.wall_hit > 0 {
        return "wallbounce";
    }
    match f.state {
        Stand => "idle",
        Walk | Roll => "walk",
        Dash | Run => "run",
        Turn | Skid => "skid",
        Crouch | JumpSquat | SpotDodge | Landing | Shield => "crouch",
        Air | AirDodge => {
            if f.vel.y < 0.0 {
                "jump"
            } else {
                "fall"
            }
        }
        LedgeHold => "hang",
        LedgeClimb => "climb",
        Jab => "jab",
        Dtilt => "dtilt",
        Nair => "nair",
        Dair => "dair",
        DashAttack => "run",
        Grab | GrabHold => "jab",   // reach / hold reuse the swing pose
        Grabbed => "skid",          // held: a stumbled pose
        Knockdown => "crouch",      // floored: low pose
        Getup => "crouch",          // rising
        TechInPlace => "skid",      // braced recovery
        TechRoll => "walk",         // rolling across the ground

        SpecialN | SpecialS | SpecialD => "jab", // reuse the swing pose until specials get art
        SpecialU => "jump",
        Helpless => "fall",
        // launched/hitstun: tumble through the air (the sim drives the slide); rising vs falling pose.
        Launched => {
            if f.vel.y < 0.0 {
                "jump"
            } else {
                "fall"
            }
        }
    }
}

/// Every clip name `clip_for` can ever return. The checker walks this so a character missing art for
/// one of them is reported once at load, not discovered as a blank frame mid-match. Keep in sync with
/// `clip_for` (adding a state there with a new clip name means adding it here).
const ALL_CLIPS: &[&str] = &[
    "idle", "walk", "run", "skid", "crouch", "jump", "fall", "hang", "climb", "jab", "nair", "dtilt",
    "dair", "wallbounce",
];

/// Where a missing clip falls back to. `clip_for` names a rich set of poses; a character need only
/// supply art for some of them, and the rest degrade down this chain to "idle" (which every
/// character must define) instead of rendering an empty frame. A name mapping to itself terminates.
pub(crate) fn clip_fallback(name: &str) -> &'static str {
    match name {
        "dtilt" => "crouch",
        "crouch" => "skid",
        "skid" => "idle",
        "nair" => "jab",
        "jab" => "idle",
        "dair" => "fall",
        "wallbounce" => "fall",
        "fall" => "jump",
        "jump" => "idle",
        "run" => "walk",
        "walk" => "idle",
        "hang" => "idle",
        "climb" => "idle",
        _ => "idle",
    }
}

/// Resolve a desired clip against what the sprite's SpriteFrames actually contains, walking the
/// fallback chain until a present animation is found ("idle" is the guaranteed terminal). Bounded so
/// a malformed chain can never spin. This is the generic safety net: `clip_for` may name a clip the
/// current character has no art for, and this picks the nearest pose it does have.
pub(crate) fn resolve_clip(a: &Gd<AnimatedSprite2D>, want: &str) -> StringName {
    let Some(sf) = a.get_sprite_frames() else { return StringName::from(want) };
    let mut name = want;
    for _ in 0..ALL_CLIPS.len() + 1 {
        if sf.has_animation(&StringName::from(name)) {
            return StringName::from(name);
        }
        let next = clip_fallback(name);
        if next == name {
            break;
        }
        name = next;
    }
    StringName::from("idle")
}

/// Multi-frame attack sync: lock the sprite's shown frame to the sim's per-state frame counter, so a
/// swing's art tracks its frame data (startup -> active -> recovery) instead of the clip's own fps
/// clock. Rollback-correct, since the shown frame is a pure function of sim state. No-op for
/// non-attacks (they keep their looping playback) and for single-frame clips. This is the hook that
/// makes genuine multi-frame attack animations land on-window; richer per-attack art just drops in.
pub(crate) fn sync_attack_frame(a: &mut Gd<AnimatedSprite2D>, f: &Fighter, t: &Tune) {
    let Some(atk) = sim::attack_for(t, f.state) else { return };
    let Some(sf) = a.get_sprite_frames() else { return };
    let name = a.get_animation();
    let n = sf.get_frame_count(&name);
    if n <= 1 {
        return; // a single-pose clip has nothing to step through
    }
    // Scrub the swing across the CURRENT live hitbox window, so a multi-box move replays the
    // animation once PER box: the 3-punch jab throws three visible punches, the stomp swings on each
    // of its three timed boxes. Before the first box: hold the wind-up frame; between/after boxes:
    // hold the last frame (the recoil) until the next box opens.
    let last = n as i64 - 1;
    let idx = if let Some(hb) = atk.box_at(f.frame) {
        let local = f.frame - hb.start; // 0..len within this box's own window
        ((local * n as i64) / hb.len.max(1)).clamp(0, last)
    } else if f.frame < atk.startup {
        0
    } else {
        last
    };
    a.set_frame(idx as i32);
}

/// Sprite tilt for a wall bounce: while the `wall_hit` window is open, lean the body off-vertical
/// (in the direction it's now travelling) so the bounce reads as a hard ricochet, easing back to
/// upright as the window decays. 0 outside the window. Cosmetic; never touches the sim.
pub(crate) fn wall_tilt(f: &Fighter) -> f32 {
    if f.wall_hit <= 0 {
        return 0.0;
    }
    let dir = if f.vel.x >= 0.0 { 1.0 } else { -1.0 };
    let decay = (f.wall_hit as f32 / 12.0).clamp(0.0, 1.0); // WALL_TILT_FRAMES in the sim
    dir * 0.45 * decay // up to ~25° at impact, unwinding to 0
}

/// Point an AnimatedSprite2D at a roster character: frames, scale, feet-offset, crisp filter, idle.
/// The single place sprite + character are wired, so ready() build and live char-swap stay in sync.
pub(crate) fn apply_character(a: &mut Gd<AnimatedSprite2D>, c: &Character) {
    let sf = build_frames(c);
    validate_character(c, &sf);
    a.set_sprite_frames(&sf);
    a.set_scale(Vector2::splat(c.scale));
    a.set_offset(Vector2::new(0.0, c.offset_y)); // feet on pos
    a.set_texture_filter(godot::classes::canvas_item::TextureFilter::NEAREST); // crisp pixels
    a.play_ex().name("idle").done();
}

/// Load a texture by path, returning None (with a warning) instead of aborting when the file is
/// missing. Lets `build_frames` skip a bad frame and fall back rather than killing the whole sprite.
pub(crate) fn try_tex(path: &str) -> Option<Gd<Texture2D>> {
    match try_load::<Texture2D>(path) {
        Ok(t) => Some(t),
        Err(_) => {
            godot_warn!("anim: missing texture {path}");
            None
        }
    }
}

/// Build a character's SpriteFrames from its clip table. Strip clips slice a sheet into cells;
/// Poses clips take one whole PNG per frame. Clip names match `clip_for` (choppy by design; the
/// art has no per-state poses, so several CharStates reuse one clip). Missing files are skipped with
/// a warning; any clip that ends up with zero frames is dropped so `resolve_clip` falls back past it.
pub(crate) fn build_frames(c: &Character) -> Gd<SpriteFrames> {
    let mut sf = SpriteFrames::new_gd();
    for clip in &c.clips {
        let name = clip.name.as_str();
        sf.add_animation(name);
        sf.set_animation_speed(name, clip.fps);
        sf.set_animation_loop(name, clip.looped);
        match &c.sheet {
            Sheet::Strip { frame_px } => {
                if let Some(sheet) = try_tex(&format!("res://assets/{}/{}.png", c.dir, clip.files[0])) {
                    // Cell width: explicit, else texture width / frame count. Cell height = full strip
                    // height, so non-square frames (common in Rivals rips) slice correctly.
                    let frames = clip.frames.max(1);
                    let fw = if *frame_px > 0.0 { *frame_px } else { sheet.get_width() as f32 / frames as f32 };
                    let fh = sheet.get_height() as f32;
                    for i in 0..frames {
                        let mut at = AtlasTexture::new_gd();
                        at.set_atlas(&sheet);
                        at.set_region(Rect2::new(Vector2::new(i as f32 * fw, 0.0), Vector2::new(fw, fh)));
                        sf.add_frame(name, &at.upcast::<Texture2D>());
                    }
                }
            }
            Sheet::Poses { prefix } => {
                for f in &clip.files {
                    if let Some(tex) = try_tex(&format!("res://assets/{}/{}_{}.png", c.dir, prefix, f)) {
                        sf.add_frame(name, &tex);
                    }
                }
            }
        }
        // a clip that loaded no frames is worse than absent: an empty animation renders nothing.
        // Drop it so `resolve_clip` walks past to a pose that has art.
        if sf.get_frame_count(name) == 0 {
            sf.remove_animation(name);
        }
    }
    sf
}

/// Startup checker: report any `clip_for` clip the built frames can't satisfy even after fallback,
/// and confirm "idle" exists (the terminal every fallback chain lands on). Logs once per character
/// at build; never panics — the game still runs on whatever art is present.
pub(crate) fn validate_character(c: &Character, sf: &Gd<SpriteFrames>) {
    if !sf.has_animation(&StringName::from("idle")) {
        godot_error!("anim: character '{}' has no 'idle' clip — fallbacks have no terminal", c.dir);
    }
    for &want in ALL_CLIPS {
        if sf.has_animation(&StringName::from(want)) {
            continue;
        }
        // walk the same chain resolve_clip uses; report what it will substitute.
        let mut name = want;
        for _ in 0..ALL_CLIPS.len() + 1 {
            let next = clip_fallback(name);
            if next == name || sf.has_animation(&StringName::from(next)) {
                name = next;
                break;
            }
            name = next;
        }
        godot_print!("anim: '{}' missing clip '{want}' -> using '{name}'", c.dir);
    }
}
