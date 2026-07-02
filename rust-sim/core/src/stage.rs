//! All surfaces in one place: the static stage (geometry, platforms, blast zones) and the drawn
//! ink-path system (Kirby-Canvas-Curse-style). A drawn path and a stage are the SAME primitive — a
//! polyline of segments — so stage geometry and ink share this module. Everything here is pure and
//! `Copy`-friendly so it rides inside the rolled-back `SimState`. Re-exported at the crate root.

use crate::{geo, Fighter, InputFrame, SimState, Tune, Vector2};
use serde::{Deserialize, Serialize};

// Battlefield-style stage: one solid main platform (with grabbable ledges) + soft platforms
// above that you land on from the top and drop through with down. All in pixel space.
pub(crate) const GROUND_Y: f32 = 760.0; // main platform top (resting feet-y)
pub(crate) const STAGE_BOTTOM: f32 = 900.0; // main platform underside (matches the Stage Main ColorRect)
pub(crate) const FLOOR_LEFT: f32 = 150.0;
pub(crate) const FLOOR_RIGHT: f32 = 1050.0; // main platform = 900 wide, centered on x=600

pub(crate) const LEDGE_REACH_X: f32 = 70.0; // how far past an edge the snap zone extends
pub(crate) const LEDGE_HANG_DY: f32 = 44.0; // hang this far below the lip while holding
// Blast zones: cross any edge = KO -> respawn. Side/top sit well outside the stage so only a
// launched (knocked-back) fighter reaches them; this is what makes horizontal/vertical knockback
// actually kill (kill moves). Bottom is the classic fall-off death.
pub const BLAST_Y: f32 = 1600.0; // below this = death (fall off the bottom)
pub const BLAST_TOP: f32 = -520.0; // above this = death (launched off the top)
pub const BLAST_LEFT: f32 = -420.0; // left of this = death
pub const BLAST_RIGHT: f32 = 1620.0; // right of this = death

/// True when a fighter has crossed any blast zone (all four edges = a real KO surface).
#[inline]
pub(crate) fn out_of_bounds(p: Vector2) -> bool {
    p.y > BLAST_Y || p.y < BLAST_TOP || p.x < BLAST_LEFT || p.x > BLAST_RIGHT
}

/// A stage platform. `solid` = the main stage (blocks, has ledges); else a soft platform
/// (land from above, drop through with down).
#[derive(Copy, Clone)]
pub struct Platform {
    pub left: f32,
    pub right: f32,
    pub y: f32,
    pub solid: bool,
}

/// Index 0 is always the solid main stage (ledges live on it). The rest are soft platforms.
pub const PLATFORMS: [Platform; 4] = [
    Platform { left: FLOOR_LEFT, right: FLOOR_RIGHT, y: GROUND_Y, solid: true },
    Platform { left: 280.0, right: 540.0, y: 575.0, solid: false }, // left
    Platform { left: 660.0, right: 920.0, y: 575.0, solid: false }, // right
    Platform { left: 470.0, right: 730.0, y: 410.0, solid: false }, // top center
];

/// A platform's top face as a `geo` segment (left..right at its y), in world space. The landing
/// path is still the closed-form AABB crossing test today; this is the same surface expressed as the
/// shape the swept `NaiveGeom::cast_shapes` landing rides on, and the seam drawn-segment stages
/// (slopes, arbitrary floors) grow from — a stage becomes a list of these instead of axis rects.
pub fn platform_top(p: &Platform) -> (geo::Iso, geo::Shape) {
    (
        geo::Iso::at(Vector2::ZERO),
        geo::Shape::Segment { a: Vector2::new(p.left, p.y), b: Vector2::new(p.right, p.y) },
    )
}

/// The solid main stage's two vertical wall faces as `geo` segments (left, then right), from the top
/// lip down to the underside. Wall collision reflects launched bodies off these (see the wall block).
pub fn stage_walls() -> [(geo::Iso, geo::Shape); 2] {
    let z = geo::Iso::at(Vector2::ZERO);
    [
        (z, geo::Shape::Segment { a: Vector2::new(FLOOR_LEFT, GROUND_Y), b: Vector2::new(FLOOR_LEFT, STAGE_BOTTOM) }),
        (z, geo::Shape::Segment { a: Vector2::new(FLOOR_RIGHT, GROUND_Y), b: Vector2::new(FLOOR_RIGHT, STAGE_BOTTOM) }),
    ]
}

// ── Drawn ink paths (Kirby-Canvas-Curse-style) ────────────────────────────────────────────────────
// A drawn path AND a stage are the same primitive: a polyline (curves flattened to segments before
// the sim ever sees them). The deterministic tick only touches segments, reusing geo.rs's segment
// math, so this stays pure with ZERO new crates. SVG authoring (usvg+lyon) would be an OFFLINE bake
// that emits these same points — never in the tick — so it can't break determinism. See the
// `ink-paths` skill for the full architecture.
pub const MAX_PATH_PTS: usize = 24; // points per path → up to MAX_PATH_PTS-1 segments
pub const MAX_DRAWN: usize = 6;     // simultaneous live paths (drawn ink + loaded stage strokes)

/// What one segment collides as. Computed ONCE at finalize by `classify` and cached on the path, so
/// the per-tick collision read is O(segments) with no trig. Grabbability lives here: a `Ledge` is a
/// `Floor` tip whose curvature (Δangle to the neighbor segment) clears `StrokeProps.ledge_curve`.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum SegClass {
    #[default]
    None,  // too short / a too-steep ramp: pass-through (no surface)
    Floor, // shallow enough to walk + land on (top face)
    Wall,  // near-vertical: blocks / reflects (only when the stroke is solid)
    Ledge, // a Floor tip with a sharp corner: grabbable lip
}

/// Per-stroke material. Plain `Copy` data stamped onto every node a tool lays, so "different pens →
/// different surfaces" needs no new state shape — just a different `StrokeProps`. Lives on the path
/// (and is editable per-tool via `DrawTool::props`).
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeProps {
    pub stroke_life: i64, // frames the whole stroke survives after it's FINISHED, then it exits at once
    pub floor_tol: f32,   // |slope angle| ≤ this ⇒ Floor (radians)
    pub wall_tol: f32,    // |slope angle| ≥ this ⇒ Wall (radians)
    pub ledge_curve: f32, // Δangle between adjacent segments at a Floor tip ≥ this ⇒ grabbable Ledge
    pub min_seg: f32,     // segments shorter than this (px) classify as None
    pub bounce: f32,      // wall restitution if `solid`
    pub density: f32,     // mass per px of stroke length (finalize: mass = Σ|seg| · density); 0 = never a body
    pub solid: bool,      // true = blocks all sides; false = soft (land from above, drop through w/ down)
    pub force_wall: bool, // classify EVERY segment as Wall (ignore slope) — a pure wall pen, no hollow bits
}

impl StrokeProps {
    /// Baseline pen: a SOLID draw-everything pen. Classifies by slope — flat is Floor (stand on it),
    /// near-vertical is Wall (blocks), the middle band is a sloped Floor (stand/slide, never a hole).
    /// `solid` means you land on it and never fade/drop through. The stroke exits a few seconds after
    /// finish. No segment is left as an empty None surface, so the whole stroke is a real collision face.
    pub const PEN: Self = Self {
        stroke_life: 240, // ~4s at 60fps after the stroke is finished, then it vanishes whole
        floor_tol: 0.55, // ~31° — flat enough to just stand
        wall_tol: 1.20,  // ~69° — steep enough to be a blocking wall
        ledge_curve: 0.7, // ~40° corner makes a lip grabbable
        min_seg: 10.0,
        bounce: 0.4,
        density: 1.0,      // 1 mass unit per px: a 300px stroke weighs 300 (kb formula rescales)
        solid: true,       // solid surface: land on it, never fade/drop through
        force_wall: false, // classify by slope (flat Floor / steep Wall / mid sloped Floor)
    };
}

/// Index into a `StrokeRegistry`'s preset table. Plain `u8` so it rides inside `Item`/`SimState` as
/// Copy data (no trait object: replay stores what HAPPENED, so the material must be resolvable from
/// data alone, not a boxed algorithm). Row 0 is always the default.
pub type StrokeId = u8;

/// How many named stroke presets the registry holds. Tune isn't in the per-frame rollback checksum
/// (it's constant config), so this can be decently wide without touching sync cost.
pub const STROKE_SLOTS: usize = 16;

/// The named-preset table of stroke materials — the "registry" a `StrokeId` resolves against. Think
/// CSS: `StrokeProps` is the property bag, this is the stylesheet, row 0 is the cascade root/default.
/// Owned by `Tune` (panel-editable), Copy + serde so it round-trips with the rest of config.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeRegistry {
    pub presets: [StrokeProps; STROKE_SLOTS],
}

impl StrokeRegistry {
    /// Every slot starts at the baseline pen; row 0 is the default. Panels/serde override rows later.
    pub const DEFAULT: Self = Self { presets: [StrokeProps::PEN; STROKE_SLOTS] };

    /// Resolve a `StrokeId` to its material, falling back to the default row (0) on an out-of-range id.
    pub fn get(&self, id: StrokeId) -> StrokeProps {
        *self.presets.get(id as usize).unwrap_or(&self.presets[0])
    }

    /// The default stroke material (row 0) — the cascade root every unstyled path inherits.
    pub fn default_props(&self) -> StrokeProps {
        self.presets[0]
    }
}

/// Which drawing tool an ink item is. Stored as plain data; behavior is static-dispatched through the
/// `DrawTool` trait so `SimState` never holds a trait object (stays `Copy` + checksummable).
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum ToolKind {
    #[default]
    TrailPen,    // lays nodes along the drawer's own movement (hold attack while you run/jump)
    CursorBrush, // stick steers a cursor offset from the body; attack plants at the cursor
    StrokeRuler, // one straight stroke per press, aimed by the stick, length = remaining budget
}

/// A drawing tool's node-placement behavior. Implemented on a zero-sized marker per kind; the sim
/// calls through the `tool_sample` shim (which `match`es on `ToolKind`) so dispatch is static and
/// state stays plain data. Material is NOT a tool concern anymore — it comes from the `StrokeRegistry`
/// keyed by the item's `StrokeId`. Add a tool = add a variant + a marker impl + a `tool_sample` arm.
pub trait DrawTool {
    /// Where (if anywhere) this tool plants a new node THIS frame, given the drawer, their input, and
    /// the path so far. `None` = lay nothing this frame. The caller enforces the length budget.
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2>;
}

pub struct TrailPen;
pub struct CursorBrush;
pub struct StrokeRuler;

impl DrawTool for TrailPen {
    fn sample(f: &Fighter, _i: &InputFrame, path: &InkPath, _t: &Tune) -> Option<Vector2> {
        // lay a node at the feet once we've moved at least one segment-length from the last node.
        let here = f.pos;
        match path.last() {
            Some(prev) if (here - prev).length() < path.props.min_seg => None,
            _ => Some(here),
        }
    }
}

impl DrawTool for CursorBrush {
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2> {
        // a cursor floats off the body in the stick direction; plant where it points.
        let aim = Vector2::new(i.dir, i.aim_y);
        let cursor = f.pos + aim * t.ink_cursor_reach;
        match path.last() {
            Some(prev) if (cursor - prev).length() < path.props.min_seg => None,
            _ => Some(cursor),
        }
    }
}

impl DrawTool for StrokeRuler {
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, _t: &Tune) -> Option<Vector2> {
        // straight stroke: from the body, step one min_seg in the aimed direction each frame until
        // budget runs out (the caller stops us). First node anchors at the body.
        let aim = Vector2::new(i.dir, i.aim_y);
        let dir = if aim.length() > 0.3 { aim / aim.length() } else { Vector2::new(f.facing, 0.0) };
        match path.last() {
            None => Some(f.pos),
            Some(prev) => Some(prev + dir * path.props.min_seg),
        }
    }
}

/// Static-dispatch shim: where (if anywhere) a tool plants a node this frame.
pub fn tool_sample(k: ToolKind, f: &Fighter, i: &InputFrame, p: &InkPath, t: &Tune) -> Option<Vector2> {
    match k {
        ToolKind::TrailPen => TrailPen::sample(f, i, p, t),
        ToolKind::CursorBrush => CursorBrush::sample(f, i, p, t),
        ToolKind::StrokeRuler => StrokeRuler::sample(f, i, p, t),
    }
}

/// One drawn polyline. Forward-packed (`pts[0..len]`); the oldest node expires first (front), the
/// newest appends at the back, so per-node decay is a front-trim + classify recompute. `Copy` +
/// fixed-cap so it rides inside `SimState` and rolls back / checksums like everything else.
///
/// Geometry is LOCAL: `pts[i]` is an offset from `pos`; `world_pt(i)` is the only world read.
/// While drawing, `pos` stays ZERO (local == world); `finalize_path` rebases to the centroid.
/// Translation-only body, no rotation (see plans/ink-billiards.md).
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkPath {
    pub pts: [Vector2; MAX_PATH_PTS],   // node offsets from `pos` (world = pts[i] + pos), 0..len
    pub born: [u64; MAX_PATH_PTS],      // global tick each node was laid (per-node expiry)
    pub class: [SegClass; MAX_PATH_PTS], // class of the segment STARTING at node i (i<len-1 valid)
    pub len: u8,                         // live node count
    pub kind: ToolKind,
    pub props: StrokeProps,
    pub owner: i8,                       // who drew it (-1 = baked stage stroke, never expires/draws)
    pub drawing: bool,                   // true while the owner is still laying it
    pub budget: f32,                     // remaining length budget (px); ≤0 = finalize
    // ── rigid body (translation only) ──
    pub pos: Vector2,                    // reference point (centroid at finalize), world space
    pub vel: Vector2,                    // px/s; ZERO = still, nonzero = traveling
    pub hp: f32,                         // damage %, scales knockback taken (fighters' formula)
    pub mass: f32,                       // Σ|seg|·density at finalize; 0.0 = not a body (baked/drawing)
}

impl InkPath {
    pub const EMPTY: Self = Self {
        pts: [Vector2::ZERO; MAX_PATH_PTS],
        born: [0; MAX_PATH_PTS],
        class: [SegClass::None; MAX_PATH_PTS],
        len: 0,
        kind: ToolKind::TrailPen,
        props: StrokeProps::PEN,
        owner: -1,
        drawing: false,
        budget: 0.0,
        pos: Vector2::ZERO,
        vel: Vector2::ZERO,
        hp: 0.0,
        mass: 0.0,
    };

    pub fn active(&self) -> bool {
        self.len > 0
    }

    /// World-space node `i`. The ONLY way to read a node as world coords.
    #[inline]
    pub fn world_pt(&self, i: usize) -> Vector2 {
        self.pts[i] + self.pos
    }

    /// World-space segment starting at node `i`. The collision / hurtbox primitive.
    #[inline]
    pub fn world_seg(&self, i: usize) -> (Vector2, Vector2) {
        (self.world_pt(i), self.world_pt(i + 1))
    }

    /// Bounding circle for coarse (billiard) passes: `pos` + the farthest local node.
    /// No thickness pad yet — the capsule radius arrives with the hurtbox work.
    pub fn bound_circle(&self) -> (Vector2, f32) {
        let r = self.pts[..self.len as usize].iter().map(|p| p.length()).fold(0.0, f32::max);
        (self.pos, r)
    }

    /// Airborne body? Baked stage (`mass == 0`) never travels.
    #[inline]
    pub fn traveling(&self) -> bool {
        self.mass > 0.0 && self.vel != Vector2::ZERO
    }

    /// The most recently laid node (world space), if any.
    pub fn last(&self) -> Option<Vector2> {
        (self.len > 0).then(|| self.world_pt(self.len as usize - 1))
    }

    /// Append a node (world coords in; stored as a `pos`-relative offset). Drops the oldest if
    /// full. Records its birth tick for per-node expiry.
    pub(crate) fn push(&mut self, p: Vector2, tick: u64) {
        if self.len as usize == MAX_PATH_PTS {
            self.trim_front(1);
        }
        let n = self.len as usize;
        self.pts[n] = p - self.pos;
        self.born[n] = tick;
        self.len += 1;
    }

    /// Drop the `k` oldest nodes (front), shifting the rest down. Keeps indexing trivial.
    pub(crate) fn trim_front(&mut self, k: usize) {
        let k = k.min(self.len as usize);
        if k == 0 {
            return;
        }
        let live = self.len as usize - k;
        for i in 0..live {
            self.pts[i] = self.pts[i + k];
            self.born[i] = self.born[i + k];
        }
        self.len = live as u8;
    }
}

/// Classify every segment of a path ONCE (at finalize, or after a node expires). This is the cached
/// grabbability the per-tick collision read consumes: each `class[i]` is the surface of the segment
/// starting at node `i`. Floor/Wall by slope tolerance; a Floor tip becomes a grabbable `Ledge` where
/// the turn to the neighbor segment is sharp enough (curvature ≥ `ledge_curve`). Pure, no per-frame
/// trig once cached.
pub fn classify(p: &mut InkPath) {
    let n = p.len as usize;
    for c in p.class.iter_mut() {
        *c = SegClass::None;
    }
    if n < 2 {
        return;
    }
    // base pass: each segment is Floor / Wall / None by its own slope. A `force_wall` stroke (the wall
    // pen) skips the slope test entirely — every real segment is Wall, so there are no hollow bits and
    // no grabbable lips (the ledge pass below is a no-op since nothing classifies as Floor).
    for s in 0..n - 1 {
        let d = p.pts[s + 1] - p.pts[s];
        let slope = d.y.atan2(d.x.abs()).abs();
        p.class[s] = if d.length() < p.props.min_seg {
            SegClass::None
        } else if p.props.force_wall {
            SegClass::Wall
        } else if slope >= p.props.wall_tol {
            SegClass::Wall
        } else {
            // Flat OR mid-slope: a Floor either way. Flat you stand on; a ramp between floor_tol and
            // wall_tol is a sloped Floor you stand/slide on — NOT a hole. `floor_tol` now only splits
            // "flat" from "sloped" for the slide accel (see the grounded-ink branch), not floor vs void.
            SegClass::Floor
        };
    }
    // ledge pass: a Floor segment whose join to the NEXT segment turns sharply (a corner, not a smooth
    // continuation) is grabbable. The two end Floor segments are also candidate lips (open ends).
    for s in 0..n - 1 {
        if p.class[s] != SegClass::Floor {
            continue;
        }
        let a = (p.pts[s + 1] - p.pts[s]).y.atan2((p.pts[s + 1] - p.pts[s]).x);
        let open_end = s == 0 || s == n - 2;
        let corner = if s + 2 < n {
            let b = (p.pts[s + 2] - p.pts[s + 1]).y.atan2((p.pts[s + 2] - p.pts[s + 1]).x);
            ang_diff(a, b) >= p.props.ledge_curve
        } else {
            false
        };
        if open_end || corner {
            p.class[s] = SegClass::Ledge;
        }
    }
}

/// y of the highest walkable (`Floor`/`Ledge`) segment of `p` directly under world-x `x`, or `None`
/// when no walkable segment spans `x`. Reads the cached `class[]` only (no trig); linear-interpolates
/// y across the spanning segment. A path still being drawn isn't yet collidable. This is the per-tick
/// collision read both the landing scan and the grounded pin consume.
pub(crate) fn ink_floor_y_at(p: &InkPath, x: f32) -> Option<f32> {
    if !p.active() || p.drawing {
        return None;
    }
    let n = p.len as usize;
    let mut best: Option<f32> = None;
    for s in 0..n.saturating_sub(1) {
        if !matches!(p.class[s], SegClass::Floor | SegClass::Ledge) {
            continue;
        }
        let (a, b) = p.world_seg(s);
        let (lo, hi) = if a.x <= b.x { (a, b) } else { (b, a) };
        if x < lo.x || x > hi.x {
            continue;
        }
        let span = hi.x - lo.x;
        let y = if span < 1e-3 { lo.y.min(hi.y) } else { lo.y + (hi.y - lo.y) * (x - lo.x) / span };
        best = Some(best.map_or(y, |by| by.min(y))); // smaller y = higher surface
    }
    best
}

/// Swept horizontal block against a path's cached `Wall` segments. `prev_x`/`pos` are the fighter's
/// feet-x before and after this frame's motion; `half_w`/`half_h` the ECB half-extents. For the
/// near-vertical Wall segment spanning the ECB's center height (`pos.y - half_h`), returns the
/// corrected feet-x (the leading side vert pinned flush to the wall) and the wall's outward normal x
/// (toward the fighter's side). The prev_x sweep catches a fast body that would tunnel entirely past
/// the wall in one frame — it's still pinned to the side it came from. Reads cached `class[]` only; a
/// path still being drawn isn't collidable. Mirrors the solid-stage side-wall block.
pub(crate) fn ink_wall_block(
    p: &InkPath,
    prev_x: f32,
    pos: Vector2,
    half_w: f32,
    half_h: f32,
) -> Option<(f32, f32)> {
    if !p.active() || p.drawing {
        return None;
    }
    let cy = pos.y - half_h; // ECB center height (matches the stage side-wall test)
    let n = p.len as usize;
    for s in 0..n.saturating_sub(1) {
        if p.class[s] != SegClass::Wall {
            continue;
        }
        let (a, b) = p.world_seg(s);
        let (lo, hi) = if a.y <= b.y { (a, b) } else { (b, a) }; // order by y
        if cy < lo.y || cy > hi.y {
            continue; // ECB center above/below this wall's vertical span
        }
        let span = hi.y - lo.y;
        // Wall class guarantees dy dominates, so x(y) along the segment is single-valued.
        let wx = if span < 1e-3 { lo.x.min(hi.x) } else { lo.x + (hi.x - lo.x) * (cy - lo.y) / span };
        // Approaching from the left: was fully left (right vert not past wx), now the right vert reaches
        // it. Pin the right vert to the wall. Symmetric for the right side. Tunneling (prev fully on one
        // side, now fully on the other) is caught by these same two tests, so it can't slip through.
        if prev_x + half_w <= wx && pos.x + half_w > wx {
            return Some((wx - half_w, -1.0));
        }
        if prev_x - half_w >= wx && pos.x - half_w < wx {
            return Some((wx + half_w, 1.0));
        }
        // Already overlapping the wall this frame (spawned inside, or a decayed reclassify): shove out
        // toward whichever side the fighter was on last frame.
        if pos.x - half_w < wx && pos.x + half_w > wx {
            let normal = if prev_x >= wx { 1.0 } else { -1.0 };
            return Some((wx + normal * half_w, normal));
        }
    }
    None
}

/// Smallest absolute angle between two headings (radians), in 0..π.
fn ang_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b).abs() % (std::f32::consts::TAU);
    if d > std::f32::consts::PI {
        d = std::f32::consts::TAU - d;
    }
    d
}

/// Post-step ink: lay/extend each drawing fighter's path (tool-specific, budget-capped), finalize a
/// path the moment its owner stops drawing or runs out of budget (running `classify` once to cache
/// grabbability), then decay old nodes per-node and free spent slots. Baked stage strokes (owner < 0)
/// never draw or decay. Pure; the only place ink paths mutate. Called last in `step`. See the
/// `ink-paths` skill.
pub(crate) fn update_paths(n: &mut SimState, inputs: &[&InputFrame], t: &Tune) {
    let tick = n.tick;
    let np = (n.active as usize).min(inputs.len());
    for idx in 0..np {
        let f = n.fighters[idx];
        let holding = f.holding;
        let pen = holding >= 0 && n.items[holding as usize].kind.is_pen();
        let inp = inputs[idx];
        let want = pen && (inp.attack || inp.attack_held);
        let active_slot = n.paths.iter().position(|p| p.drawing && p.owner == idx as i8);
        if want {
            let tool = n.items[holding as usize].tool;
            let slot = match active_slot {
                Some(s) => s,
                None => {
                    let Some(s) = n.paths.iter().position(|p| !p.active() && !p.drawing) else {
                        continue; // no free path slot — drop the stroke
                    };
                    let mut fresh = InkPath::EMPTY;
                    fresh.kind = tool;
                    fresh.props = t.strokes.get(n.items[holding as usize].stroke); // registry lookup by StrokeId
                    fresh.owner = idx as i8;
                    fresh.drawing = true;
                    // Each stroke draws from the pen's remaining gas (its total ink), not a fresh
                    // per-stroke budget, so gas depletes across strokes and the overhead bar means
                    // "ink left". The pen is spent (vanishes) when gas hits zero, like a gun.
                    fresh.budget = n.items[holding as usize].gas;
                    n.paths[s] = fresh;
                    s
                }
            };
            let path = n.paths[slot];
            if let Some(p) = tool_sample(tool, &f, inp, &path, t) {
                let add = path.last().map_or(0.0, |prev| (p - prev).length());
                if path.len == 0 || (add > 0.0 && path.budget - add >= 0.0) {
                    n.paths[slot].push(p, tick);
                    n.paths[slot].budget -= add;
                    n.items[holding as usize].gas -= add; // deplete the pen's total gas (HUD bar)
                }
                if n.paths[slot].budget <= 0.0 {
                    finalize_path(&mut n.paths[slot]); // stroke's gas spent: solidify
                }
                if n.items[holding as usize].gas <= 0.0 {
                    // pen out of ink: don't pop instantly like a spent gun — detach it to the ground
                    // (still briefly pickup-able). `update_items` despawns it once it settles idle on
                    // the floor with no ink left (its "unload").
                    n.items[holding as usize].owner = -1;
                    n.fighters[idx].holding = -1;
                }
            }
        } else if let Some(s) = active_slot {
            finalize_path(&mut n.paths[s]); // released the button: solidify
        }
    }

    // whole-stroke decay: a FINISHED stroke lives `stroke_life` frames past its last-laid node, then
    // exits all at once (the shell shows the countdown above it). Still-drawing strokes and baked
    // stage strokes (owner < 0) never expire. The timer is anchored on the newest node's birth, so it
    // starts counting from the moment the owner stops extending the stroke.
    for p in n.paths.iter_mut() {
        if !p.active() || p.owner < 0 || p.drawing {
            continue;
        }
        let newest = p.born[p.len as usize - 1];
        if tick.saturating_sub(newest) > p.props.stroke_life as u64 {
            *p = InkPath::EMPTY;
        }
    }
}

/// Stop drawing a path and cache its per-segment surface classes (the grabbability the collision read
/// consumes). Called on button release or budget exhaustion. Also rebases the geometry: `pos` becomes
/// the node centroid and `pts` become offsets from it, leaving every `world_pt` identical up to f32
/// rounding (pure translation bookkeeping — it lets the whole path move later by writing `pos` alone).
fn finalize_path(p: &mut InkPath) {
    p.drawing = false;
    let n = p.len as usize;
    if n > 0 {
        let c = p.pts[..n].iter().copied().fold(Vector2::ZERO, |a, b| a + b) / n as f32 + p.pos;
        let shift = p.pos - c;
        for q in p.pts[..n].iter_mut() {
            *q += shift;
        }
        p.pos = c;
    }
    // body mass: stroke length × material density. density 0 (baked stage presets) leaves
    // mass 0 = the not-a-body sentinel, so those strokes can never be knocked into Traveling.
    p.mass = (0..n.saturating_sub(1))
        .map(|s| (p.pts[s + 1] - p.pts[s]).length())
        .sum::<f32>()
        * p.props.density;
    classify(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Push some world points as a finished (non-drawing) stroke and classify, WITHOUT the
    /// finalize rebase — the "old representation" (pos = ZERO, pts world) as a control.
    fn raw_path(world: &[Vector2]) -> InkPath {
        let mut p = InkPath::EMPTY;
        p.owner = 0;
        for (i, w) in world.iter().enumerate() {
            p.push(*w, i as u64);
        }
        classify(&mut p);
        p
    }

    #[test]
    fn finalize_rebases_pos_to_centroid_without_moving_world_geometry() {
        let world =
            [Vector2::new(100.0, 50.0), Vector2::new(220.0, 64.0), Vector2::new(300.0, 40.0)];
        let mut p = InkPath::EMPTY;
        p.owner = 0;
        p.drawing = true;
        for (i, w) in world.iter().enumerate() {
            p.push(*w, i as u64);
        }
        assert_eq!(p.pos, Vector2::ZERO, "while drawing local == world");
        finalize_path(&mut p);
        let c = (world[0] + world[1] + world[2]) / 3.0;
        assert!((p.pos - c).length() < 1e-3, "pos is the node centroid");
        for (i, w) in world.iter().enumerate() {
            assert!((p.world_pt(i) - *w).length() < 1e-3, "world_pt {i} unmoved by rebase");
        }
    }

    #[test]
    fn rebased_path_collides_identically_to_world_space_path() {
        let world = [
            Vector2::new(100.0, 400.0),
            Vector2::new(260.0, 400.0), // flat floor span
            Vector2::new(262.0, 250.0), // near-vertical wall up
        ];
        let control = raw_path(&world); // pos = ZERO, pts world (the old representation)
        let mut rebased = control;
        finalize_path(&mut rebased);
        for x in [100.0f32, 150.0, 200.0, 259.0] {
            let a = ink_floor_y_at(&control, x);
            let b = ink_floor_y_at(&rebased, x);
            match (a, b) {
                (Some(ya), Some(yb)) => assert!((ya - yb).abs() < 1e-3, "floor y at {x}"),
                (a, b) => assert_eq!(a.is_some(), b.is_some(), "floor presence at {x}"),
            }
        }
        // wall block: approach the near-vertical segment from the left at its mid height
        let hit_c = ink_wall_block(&control, 220.0, Vector2::new(258.0, 340.0), 10.0, 20.0);
        let hit_r = ink_wall_block(&rebased, 220.0, Vector2::new(258.0, 340.0), 10.0, 20.0);
        match (hit_c, hit_r) {
            (Some((xa, na)), Some((xb, nb))) => {
                assert!((xa - xb).abs() < 1e-3);
                assert_eq!(na, nb);
            }
            (a, b) => assert_eq!(a.is_some(), b.is_some(), "wall block presence"),
        }
    }


    #[test]
    fn finalize_computes_mass_from_length_times_density() {
        let world =
            [Vector2::new(0.0, 0.0), Vector2::new(100.0, 0.0), Vector2::new(100.0, 50.0)];
        let mut p = InkPath::EMPTY;
        p.owner = 0;
        p.drawing = true;
        for (i, w) in world.iter().enumerate() {
            p.push(*w, i as u64);
        }
        assert_eq!(p.mass, 0.0, "no mass while drawing");
        finalize_path(&mut p);
        assert!((p.mass - 150.0 * p.props.density).abs() < 1e-2, "mass = length x density, got {}", p.mass);

        // a zero-density material never becomes a body, even finalized (baked stage preset shape)
        let mut baked = InkPath::EMPTY;
        baked.props.density = 0.0;
        for (i, w) in world.iter().enumerate() {
            baked.push(*w, i as u64);
        }
        finalize_path(&mut baked);
        assert_eq!(baked.mass, 0.0);
        assert!(!baked.traveling());
    }

    #[test]
    fn empty_and_baked_paths_are_not_bodies() {
        let p = InkPath::EMPTY;
        assert_eq!(p.mass, 0.0);
        assert!(!p.traveling());
        let mut moving = p;
        moving.vel = Vector2::new(5.0, 0.0);
        assert!(!moving.traveling(), "mass 0 never travels even with vel set");
    }
}
