//! All surfaces in one place: the static stage (geometry, platforms, blast zones) and the drawn
//! ink-path system (Kirby-Canvas-Curse-style). A drawn path and a stage are the SAME primitive — a
//! polyline of segments — so stage geometry and ink share this module. Everything here is pure and
//! `Copy`-friendly so it rides inside the rolled-back `SimState`. Re-exported at the crate root.

use crate::{geo, Fighter, InputFrame, Tune, Vector2};
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
    pub node_life: i64,   // frames each NODE survives once laid (the "nodes go away after time N" decay)
    pub floor_tol: f32,   // |slope angle| ≤ this ⇒ Floor (radians)
    pub wall_tol: f32,    // |slope angle| ≥ this ⇒ Wall (radians)
    pub ledge_curve: f32, // Δangle between adjacent segments at a Floor tip ≥ this ⇒ grabbable Ledge
    pub min_seg: f32,     // segments shorter than this (px) classify as None
    pub bounce: f32,      // wall restitution if `solid`
    pub solid: bool,      // true = blocks all sides; false = soft (land from above, drop through w/ down)
}

impl StrokeProps {
    /// Baseline pen: soft platform with grabbable lips, fades a few seconds after each node is laid.
    pub const PEN: Self = Self {
        node_life: 240, // ~4s at 60fps before a node fades
        floor_tol: 0.55, // ~31° — walkable
        wall_tol: 1.20,  // ~69° — wall
        ledge_curve: 0.7, // ~40° corner makes a lip grabbable
        min_seg: 10.0,
        bounce: 0.4,
        solid: false, // soft + grabbable lips (the chosen default)
    };
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

/// A drawing tool's behavior. Implemented on a zero-sized marker per kind; the sim calls through the
/// `tool_*` shims (which `match` on `ToolKind`) so dispatch is static and state stays plain data.
/// Pure: every method is value-in / value-out. Add a tool = add a variant + a marker impl + a match
/// arm in the two shims. See the `ink-paths` skill.
pub trait DrawTool {
    /// The material this tool stamps on the nodes it lays.
    fn props(t: &Tune) -> StrokeProps;
    /// Where (if anywhere) this tool plants a new node THIS frame, given the drawer, their input, and
    /// the path so far. `None` = lay nothing this frame. The caller enforces the length budget.
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2>;
}

pub struct TrailPen;
pub struct CursorBrush;
pub struct StrokeRuler;

impl DrawTool for TrailPen {
    fn props(t: &Tune) -> StrokeProps {
        t.pen
    }
    fn sample(f: &Fighter, _i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2> {
        // lay a node at the feet once we've moved at least one segment-length from the last node.
        let here = f.pos;
        match path.last() {
            Some(prev) if (here - prev).length() < t.pen.min_seg => None,
            _ => Some(here),
        }
    }
}

impl DrawTool for CursorBrush {
    fn props(t: &Tune) -> StrokeProps {
        t.pen
    }
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2> {
        // a cursor floats off the body in the stick direction; plant where it points.
        let aim = Vector2::new(i.dir, i.aim_y);
        let cursor = f.pos + aim * t.ink_cursor_reach;
        match path.last() {
            Some(prev) if (cursor - prev).length() < t.pen.min_seg => None,
            _ => Some(cursor),
        }
    }
}

impl DrawTool for StrokeRuler {
    fn props(t: &Tune) -> StrokeProps {
        t.pen
    }
    fn sample(f: &Fighter, i: &InputFrame, path: &InkPath, t: &Tune) -> Option<Vector2> {
        // straight stroke: from the body, step one min_seg in the aimed direction each frame until
        // budget runs out (the caller stops us). First node anchors at the body.
        let aim = Vector2::new(i.dir, i.aim_y);
        let dir = if aim.length() > 0.3 { aim / aim.length() } else { Vector2::new(f.facing, 0.0) };
        match path.last() {
            None => Some(f.pos),
            Some(prev) => Some(prev + dir * t.pen.min_seg),
        }
    }
}

/// Static-dispatch shim: a tool's stroke material.
pub fn tool_props(k: ToolKind, t: &Tune) -> StrokeProps {
    match k {
        ToolKind::TrailPen => TrailPen::props(t),
        ToolKind::CursorBrush => CursorBrush::props(t),
        ToolKind::StrokeRuler => StrokeRuler::props(t),
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
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkPath {
    pub pts: [Vector2; MAX_PATH_PTS],   // node positions, world space, indices 0..len
    pub born: [u64; MAX_PATH_PTS],      // global tick each node was laid (per-node expiry)
    pub class: [SegClass; MAX_PATH_PTS], // class of the segment STARTING at node i (i<len-1 valid)
    pub len: u8,                         // live node count
    pub kind: ToolKind,
    pub props: StrokeProps,
    pub owner: i8,                       // who drew it (-1 = baked stage stroke, never expires/draws)
    pub drawing: bool,                   // true while the owner is still laying it
    pub budget: f32,                     // remaining length budget (px); ≤0 = finalize
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
    };

    pub fn active(&self) -> bool {
        self.len > 0
    }

    /// The most recently laid node, if any.
    pub fn last(&self) -> Option<Vector2> {
        (self.len > 0).then(|| self.pts[self.len as usize - 1])
    }

    /// Append a node (drops the oldest if full). Records its birth tick for per-node expiry.
    pub(crate) fn push(&mut self, p: Vector2, tick: u64) {
        if self.len as usize == MAX_PATH_PTS {
            self.trim_front(1);
        }
        let n = self.len as usize;
        self.pts[n] = p;
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
    // base pass: each segment is Floor / Wall / None by its own slope.
    for s in 0..n - 1 {
        let d = p.pts[s + 1] - p.pts[s];
        let slope = d.y.atan2(d.x.abs()).abs();
        p.class[s] = if d.length() < p.props.min_seg {
            SegClass::None
        } else if slope <= p.props.floor_tol {
            SegClass::Floor
        } else if slope >= p.props.wall_tol {
            SegClass::Wall
        } else {
            SegClass::None // a ramp too steep to stand on but not vertical: slide-off, no surface
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
        let (a, b) = (p.pts[s], p.pts[s + 1]);
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

/// Smallest absolute angle between two headings (radians), in 0..π.
fn ang_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b).abs() % (std::f32::consts::TAU);
    if d > std::f32::consts::PI {
        d = std::f32::consts::TAU - d;
    }
    d
}
