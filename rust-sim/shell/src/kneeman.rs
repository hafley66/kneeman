use futures_signals::signal::Mutable;
use godot::classes::{
    AnimatedSprite2D, AtlasTexture, ColorRect, INode2D, Input, Node2D, SpriteFrames, Texture2D,
};
use godot::prelude::*;
use godot::tools::load;

use crate::sim::{self, CharState, Fighter, InputFrame, SimState, Tune};

/// CC0 "Pixel Adventure" character (Pixel Frog). Each animation is one horizontal strip of
/// 32x32 frames; we slice them into SpriteFrames clips. Swap to any sibling under
/// assets/pixelfrog ("ninjafrog", "maskdude", "pinkman", "virtualguy") — same strips/counts.
/// Test art only — CC0, not a shipping character.
const SPRITE_DIR: &str = "pixelfrog/ninjafrog";
const SPRITE_FRAME: f32 = 32.0; // source cell size (square)
const SPRITE_SCALE: f32 = 3.3; // 32px art -> ~105px tall, near the ECB height
const SPRITE_OFFSET_Y: f32 = -12.0; // seat the frog's feet on pos (scale-invariant)

/// Boundary: the pure sim speaks glam::Vec2; Godot wants its own Vector2. Convert on the way out.
#[inline]
fn gv(v: sim::Vector2) -> Vector2 {
    Vector2::new(v.x, v.y)
}

/// KneeMan: the impure SHELL around the pure sim.
/// Owns the BehaviorSubjects (state + tune). Each tick: sample input -> pure step ->
/// publish into the state cell -> render (position + sprite clip). That is the only
/// place effects live; `sim::step` itself is pure.
#[derive(GodotClass)]
#[class(base = Node2D)]
pub struct KneeMan {
    base: Base<Node2D>,
    state: Mutable<SimState>, // source of truth, observed everywhere
    tune: Mutable<Tune>,      // live config, written by egui
    anim: Option<Gd<AnimatedSprite2D>>, // the sprite we drive by CharState
    dummy: Option<Gd<ColorRect>>,       // training-dummy block (sibling node)
}

#[godot_api]
impl INode2D for KneeMan {
    fn init(base: Base<Node2D>) -> Self {
        Self {
            base,
            state: Mutable::new(SimState::spawn()),
            tune: Mutable::new(Tune::default()),
            anim: None,
            dummy: None,
        }
    }

    fn ready(&mut self) {
        let pos = self.state.get().fighters[0].pos;
        self.base_mut().set_position(gv(pos));

        // Resolve the AnimatedSprite2D and hand it a SpriteFrames built in code
        // (one clip per movement state). Building it here, not in a .tres, keeps the
        // CharState->clip wiring readable and avoids hand-authoring resource uids.
        if let Some(mut a) = self
            .base()
            .get_node_or_null("Anim")
            .and_then(|n| n.try_cast::<AnimatedSprite2D>().ok())
        {
            a.set_sprite_frames(&build_frames());
            a.set_scale(Vector2::splat(SPRITE_SCALE));
            a.set_offset(Vector2::new(0.0, SPRITE_OFFSET_Y)); // feet on pos
            a.set_texture_filter(godot::classes::canvas_item::TextureFilter::NEAREST); // crisp pixels
            a.play_ex().name("idle").done();
            self.anim = Some(a);
        }

        self.dummy = self
            .base()
            .get_node_or_null("../Dummy")
            .and_then(|n| n.try_cast::<ColorRect>().ok());
    }

    fn physics_process(&mut self, _delta: f64) {
        let input = Input::singleton();
        let frame = InputFrame {
            dir: input.get_axis("ui_left", "ui_right"),
            aim_y: input.get_axis("ui_up", "ui_down"), // -1 up .. +1 down
            jump: input.is_action_just_pressed("ui_accept")
                || input.is_action_just_pressed("ui_up"),
            jump_held: input.is_action_pressed("ui_accept") || input.is_action_pressed("ui_up"),
            shorthop: input.is_action_just_pressed("shorthop"),
            shield_held: input.is_action_pressed("shield"),
            shield_pressed: input.is_action_just_pressed("shield"),
            down: input.is_action_pressed("ui_down"),
            down_pressed: input.is_action_just_pressed("ui_down"),
            attack: input.is_action_just_pressed("attack"),
        };

        // fighter[0] = this keyboard; fighter[1] = neutral for now (a real, knockable fighter
        // that just stands still — the training dummy, promoted. Real P2 input lands in M3.)
        let p1 = InputFrame::default();
        let next = sim::step(&self.state.get(), [&frame, &p1], &self.tune.get()); // pure scan
        self.state.set(next); // publish (notifies observers)
        self.base_mut().set_position(gv(next.fighters[0].pos)); // render (subscribe)
        self.render_anim(&next.fighters[0]); // render (subscribe)
        self.render_dummy(&next.fighters[1]); // render (subscribe)
        self.base_mut().queue_redraw(); // refresh the debug box overlay
    }

    /// Debug overlay: active hitbox (red) + dummy hurtbox (yellow). Coordinates are world,
    /// converted to this node's local space (the node sits at the player position).
    fn draw(&mut self) {
        let s = self.state.get();
        let t = self.tune.get();
        let origin = self.base().get_position();

        // ECB diamond (cyan): the actual collision shape — bottom vert = feet, side verts = walls.
        let v = sim::ecb_verts(s.fighters[0].pos);
        let ecb = Color::from_rgba(0.20, 0.85, 0.95, 0.85);
        for k in 0..4 {
            let a = gv(v[k]) - origin;
            let b = gv(v[(k + 1) % 4]) - origin;
            self.base_mut().draw_line_ex(a, b, ecb).width(2.0).done();
        }

        // fighter[1] hurtbox (yellow): the opponent/dummy circle the attack lands on.
        let (bc, br) = sim::hurtbox(&s.fighters[1]);
        let hurt = gv(bc) - origin;
        self.base_mut()
            .draw_circle(hurt, br, Color::from_rgba(0.95, 0.85, 0.20, 0.30));
        if let Some((hc, hr)) = sim::active_hitbox(&s.fighters[0], &t) {
            let c = gv(hc) - origin;
            self.base_mut()
                .draw_circle(c, hr, Color::from_rgba(0.95, 0.25, 0.25, 0.45));
        }
    }
}

impl KneeMan {
    /// Hand out the shared cells (clones point at the same BehaviorSubject).
    pub fn state_cell(&self) -> Mutable<SimState> {
        self.state.clone()
    }

    pub fn tune_cell(&self) -> Mutable<Tune> {
        self.tune.clone()
    }

    /// Drive the sprite: pick the clip for the state, flip by facing, tint green while
    /// intangible (the universal "you can't be hit" read).
    fn render_anim(&mut self, f: &Fighter) {
        let Some(mut a) = self.anim.clone() else { return };
        let clip = clip_for(f);
        if a.get_animation() != StringName::from(clip) {
            a.play_ex().name(clip).done(); // only restart when the clip actually changes
        }
        a.set_flip_h(f.facing < 0.0); // frog faces right by default
        let tint = if f.intangible {
            Color::from_rgb(0.30, 0.95, 0.40)
        } else {
            Color::WHITE
        };
        a.set_modulate(tint);
    }

    /// Move the dummy block to its sim position (ColorRect origin is top-left, so center it),
    /// and flash it white-hot during hitstun.
    fn render_dummy(&mut self, f: &Fighter) {
        let Some(mut d) = self.dummy.clone() else { return };
        let size = d.get_size();
        let (center, _) = sim::hurtbox(f); // center the block on the body
        d.set_position(gv(center) - size * 0.5);
        let col = if f.hitstun > 0 {
            Color::from_rgb(1.0, 0.95, 0.55) // hit flash
        } else {
            Color::from_rgb(0.45, 0.45, 0.55)
        };
        d.set_color(col);
    }
}

/// CharState -> SpriteFrames clip. 15 states collapse onto ~9 clips (choppy by design;
/// the Kenney pose set has no per-state art). Air splits rise/fall by vertical velocity.
fn clip_for(f: &Fighter) -> &'static str {
    use CharState::*;
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
        Nair => "nair",
    }
}

/// Build SpriteFrames by slicing the frog's animation strips. Our 11 movement clips map onto the
/// 7 frog animations (choppy by design; no per-state art): walk/run reuse Run, crouch/skid reuse
/// Fall, hang reuses the Wall-Jump cling, climb/nair reuse the Double-Jump flip, jab reuses Hit.
fn build_frames() -> Gd<SpriteFrames> {
    let mut sf = SpriteFrames::new_gd();
    add_strip(&mut sf, "idle", "idle", 11, 14.0, true);
    add_strip(&mut sf, "walk", "run", 12, 14.0, true);
    add_strip(&mut sf, "run", "run", 12, 20.0, true);
    add_strip(&mut sf, "crouch", "fall", 1, 1.0, false);
    add_strip(&mut sf, "skid", "fall", 1, 1.0, false);
    add_strip(&mut sf, "jump", "jump", 1, 1.0, false);
    add_strip(&mut sf, "fall", "fall", 1, 1.0, false);
    add_strip(&mut sf, "hang", "wall_jump", 5, 12.0, true);
    add_strip(&mut sf, "climb", "double_jump", 6, 14.0, true);
    add_strip(&mut sf, "jab", "hit", 7, 20.0, false);
    add_strip(&mut sf, "nair", "double_jump", 6, 18.0, true);
    sf
}

/// One clip from a horizontal strip: load the sheet once, add an AtlasTexture per 32x32 cell.
fn add_strip(sf: &mut Gd<SpriteFrames>, name: &str, file: &str, frames: i32, fps: f64, looped: bool) {
    sf.add_animation(name);
    sf.set_animation_speed(name, fps);
    sf.set_animation_loop(name, looped);
    let sheet = load::<Texture2D>(&format!("res://assets/{SPRITE_DIR}/{file}.png"));
    for i in 0..frames {
        let mut at = AtlasTexture::new_gd();
        at.set_atlas(&sheet);
        at.set_region(Rect2::new(
            Vector2::new(i as f32 * SPRITE_FRAME, 0.0),
            Vector2::new(SPRITE_FRAME, SPRITE_FRAME),
        ));
        let tex = at.upcast::<Texture2D>();
        sf.add_frame(name, &tex);
    }
}
