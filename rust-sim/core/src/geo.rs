//! Deterministic 2D collision geometry, shaped to mirror `parry2d::query` so the backend can be
//! swapped for parry later by writing ONE impl, not editing call sites.
//!
//! The contract matches parry's: every query takes two `(Iso, Shape)` pairs and returns parry's
//! result types (`Contact`, `ShapeCastHit`) with parry's field names. `NaiveGeom` is the in-house
//! closed-form backend used today (glam f32, sqrt-free where it can be, no trig -> as deterministic
//! as the rest of the sim, which the synctest gate verifies). When we want parry's full shape zoo /
//! robust TOI, `ParryGeom` wraps `parry2d::query::*`, converts glam<->nalgebra, and the sim is
//! untouched because it only ever names `impl Geometry`, `Shape`, `Contact`, `ShapeCastHit`.
//!
//! Mapping to parry, so the swap is mechanical:
//!   Iso              <-> parry2d::math::Isometry      (translation + rotation)
//!   Shape::Ball      <-> parry2d::shape::Ball
//!   Shape::Cuboid    <-> parry2d::shape::Cuboid       (half_extents)
//!   Shape::Segment   <-> parry2d::shape::Segment
//!   Shape::Capsule   <-> parry2d::shape::Capsule
//!   Contact          <-> parry2d::query::Contact      (point1, point2, normal1, dist)
//!   ShapeCastHit     <-> parry2d::query::ShapeCastHit (time_of_impact, witness1, normal1)
//!   Geometry::intersection_test/distance/contact/cast_shapes <-> the parry2d::query free fns

use crate::Vector2; // = glam::Vec2

const EPS: f32 = 1e-6;

/// Placement of a shape: translation + rotation (radians). Mirrors `parry2d::math::Isometry`.
/// Most fighter shapes are axis-aligned (`rot == 0`); rotation is kept for API parity + spun hitboxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Iso {
    pub pos: Vector2,
    pub rot: f32,
}

impl Iso {
    /// Translation only (rot = 0) -- the common case.
    pub fn at(pos: Vector2) -> Self {
        Self { pos, rot: 0.0 }
    }
    pub fn new(pos: Vector2, rot: f32) -> Self {
        Self { pos, rot }
    }
    /// Map a shape-local point into world space.
    pub fn apply(&self, local: Vector2) -> Vector2 {
        if self.rot == 0.0 {
            self.pos + local
        } else {
            let (s, c) = self.rot.sin_cos();
            self.pos + Vector2::new(c * local.x - s * local.y, s * local.x + c * local.y)
        }
    }
}

/// The shapes the sim uses, each a 1:1 with a parry2d shape. Segment/Capsule endpoints are in the
/// shape's LOCAL frame (placed by the query's `Iso`); Ball/Cuboid are centered on the `Iso`.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Ball { r: f32 },
    Cuboid { half: Vector2 },
    Segment { a: Vector2, b: Vector2 },
    Capsule { a: Vector2, b: Vector2, r: f32 },
}

/// Closest-points contact between two shapes. Mirrors `parry2d::query::Contact`.
/// `normal1` points from shape 1 toward shape 2. `dist` is signed: negative = penetration depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub point1: Vector2,
    pub point2: Vector2,
    pub normal1: Vector2,
    pub dist: f32,
}

/// Swept (time-of-impact) result. Mirrors `parry2d::query::ShapeCastHit`.
/// `time_of_impact` is in the same units as the velocities passed to `cast_shapes` (fraction of the
/// step when called with per-step displacement). `witness1` is the contact point on the mover.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeCastHit {
    pub time_of_impact: f32,
    pub witness1: Vector2,
    pub normal1: Vector2,
}

/// The collision backend. Method names + argument shapes mirror `parry2d::query` so `ParryGeom` is a
/// thin delegating wrapper. The sim holds `impl Geometry` and never names a concrete backend.
pub trait Geometry {
    /// Do the two placed shapes overlap? `parry2d::query::intersection_test`.
    fn intersection_test(&self, a: (Iso, Shape), b: (Iso, Shape)) -> bool;
    /// Separation distance (0.0 if overlapping). `parry2d::query::distance`.
    fn distance(&self, a: (Iso, Shape), b: (Iso, Shape)) -> f32;
    /// Closest-points contact if within `prediction`. `parry2d::query::contact`.
    fn contact(&self, a: (Iso, Shape), b: (Iso, Shape), prediction: f32) -> Option<Contact>;
    /// Swept query: move shape `a` by `vel_a` and `b` by `vel_b`, first touch within `max_toi`.
    /// `parry2d::query::cast_shapes`. Used for tunneling-safe landings (feet crossing a thin top).
    fn cast_shapes(
        &self,
        a: (Iso, Shape),
        vel_a: Vector2,
        b: (Iso, Shape),
        vel_b: Vector2,
        max_toi: f32,
    ) -> Option<ShapeCastHit>;
}

/// In-house closed-form backend (no external deps, deterministic). Zero-sized: free to pass around.
#[derive(Clone, Copy, Default)]
pub struct NaiveGeom;

impl Geometry for NaiveGeom {
    fn intersection_test(&self, a: (Iso, Shape), b: (Iso, Shape)) -> bool {
        self.distance(a, b) <= EPS
    }

    fn distance(&self, a: (Iso, Shape), b: (Iso, Shape)) -> f32 {
        self.contact(a, b, f32::MAX).map(|c| c.dist.max(0.0)).unwrap_or(f32::MAX)
    }

    fn contact(&self, a: (Iso, Shape), b: (Iso, Shape), prediction: f32) -> Option<Contact> {
        // Round shapes (Ball / Segment / Capsule) all reduce to "a core segment + a radius", so one
        // code path covers every pairing among them: closest points of the cores, then offset by the
        // radii. Cuboid decomposes into its 4 edge segments (closest of the cores wins). This is the
        // same "round shape" trick parry uses internally.
        let cores_a = cores(a.0, a.1);
        let cores_b = cores(b.0, b.1);
        let mut best: Option<Contact> = None;
        for &(a0, a1, ra) in &cores_a {
            for &(b0, b1, rb) in &cores_b {
                let (p1, p2) = closest_seg_seg(a0, a1, b0, b1);
                let delta = p2 - p1;
                let d = delta.length();
                let n = if d > EPS { delta / d } else { default_normal(a0, a1) };
                let c = Contact {
                    point1: p1 + n * ra,
                    point2: p2 - n * rb,
                    normal1: n,
                    dist: d - (ra + rb),
                };
                if best.map_or(true, |bc| c.dist < bc.dist) {
                    best = Some(c);
                }
            }
        }
        best.filter(|c| c.dist <= prediction)
    }

    fn cast_shapes(
        &self,
        a: (Iso, Shape),
        vel_a: Vector2,
        b: (Iso, Shape),
        vel_b: Vector2,
        max_toi: f32,
    ) -> Option<ShapeCastHit> {
        // Reduce to b-static by working in b's frame: only the relative velocity matters.
        let rel = vel_a - vel_b;
        if rel.length_squared() < EPS {
            return None;
        }
        // Each shape is round cores (center+radius). A core of `a` sweeping vs a core of `b` touches
        // when the moving core's center comes within (ra+rb) of b's core segment. With b static that
        // is: ray (a-center, rel) vs b-core inflated by (ra+rb). Take the earliest over all core pairs.
        let cores_a = cores(a.0, a.1);
        let cores_b = cores(b.0, b.1);
        let mut best: Option<ShapeCastHit> = None;
        for &(a0, a1, ra) in &cores_a {
            // Sweep each endpoint of a's core (covers Ball: a0==a1; Segment/Capsule: both ends).
            for ca in [a0, a1] {
                for &(b0, b1, rb) in &cores_b {
                    if let Some(t) = ray_vs_capsule(ca, rel, b0, b1, ra + rb, max_toi) {
                        if best.map_or(true, |h| t < h.time_of_impact) {
                            let hit = ca + rel * t;
                            let on_b = closest_on_seg(hit, b0, b1);
                            let n = (hit - on_b).normalize_or_zero();
                            best = Some(ShapeCastHit {
                                time_of_impact: t,
                                witness1: hit - n * ra,
                                normal1: n,
                            });
                        }
                    }
                }
            }
        }
        best
    }
}

/// A shape's "round cores": list of (segment-start, segment-end, radius) in WORLD coords. Ball is a
/// degenerate segment (a==b); Cuboid is its 4 zero-radius edges. The pile of cores is what the
/// closest-point / sweep loops iterate, so every shape pairing falls out of segment-vs-segment.
fn cores(iso: Iso, shape: Shape) -> Vec<(Vector2, Vector2, f32)> {
    match shape {
        Shape::Ball { r } => vec![(iso.pos, iso.pos, r)],
        Shape::Capsule { a, b, r } => vec![(iso.apply(a), iso.apply(b), r)],
        Shape::Segment { a, b } => vec![(iso.apply(a), iso.apply(b), 0.0)],
        Shape::Cuboid { half } => {
            let c = [
                iso.apply(Vector2::new(-half.x, -half.y)),
                iso.apply(Vector2::new(half.x, -half.y)),
                iso.apply(Vector2::new(half.x, half.y)),
                iso.apply(Vector2::new(-half.x, half.y)),
            ];
            vec![
                (c[0], c[1], 0.0),
                (c[1], c[2], 0.0),
                (c[2], c[3], 0.0),
                (c[3], c[0], 0.0),
            ]
        }
    }
}

/// A stable fallback normal when two cores are exactly coincident (pick the segment's perpendicular).
fn default_normal(a0: Vector2, a1: Vector2) -> Vector2 {
    let d = a1 - a0;
    if d.length_squared() > EPS {
        d.perp().normalize_or_zero()
    } else {
        Vector2::new(0.0, -1.0)
    }
}

/// Closest point on segment [a,b] to p (clamped projection).
pub fn closest_on_seg(p: Vector2, a: Vector2, b: Vector2) -> Vector2 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < EPS {
        return a;
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    a + ab * t
}

/// Closest pair of points between segments [p1,q1] and [p2,q2]. Ericson, Real-Time Collision
/// Detection §5.1.9 -- closed form, no trig. Returns (point on seg 1, point on seg 2).
pub fn closest_seg_seg(p1: Vector2, q1: Vector2, p2: Vector2, q2: Vector2) -> (Vector2, Vector2) {
    let d1 = q1 - p1; // direction of segment 1
    let d2 = q2 - p2; // direction of segment 2
    let r = p1 - p2;
    let a = d1.length_squared();
    let e = d2.length_squared();
    let f = d2.dot(r);

    let (mut s, mut t);
    if a < EPS && e < EPS {
        return (p1, p2); // both degenerate to points
    }
    if a < EPS {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e < EPS {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            s = if denom > EPS { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

/// Time `t` in [0, max] at which a point leaving `origin` along `dir` first comes within `radius`
/// of segment [s0,s1] (i.e. enters the capsule of that segment). None if it never does in range.
/// This is the swept-circle-vs-segment primitive landings ride on. Sampling-free: solves the two
/// endpoint circles and the infinite-slab band, takes the earliest valid hit.
fn ray_vs_capsule(
    origin: Vector2,
    dir: Vector2,
    s0: Vector2,
    s1: Vector2,
    radius: f32,
    max: f32,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    let mut keep = |t: f32| {
        if (0.0..=max).contains(&t) {
            best = Some(best.map_or(t, |b: f32| b.min(t)));
        }
    };
    // endpoint discs
    for &c in &[s0, s1] {
        if let Some(t) = ray_vs_circle(origin, dir, c, radius) {
            keep(t);
        }
    }
    // the band: offset the segment by +/-radius along its normal, intersect the ray with the two
    // offset segments. Covers the flat face of the capsule between the endpoint discs.
    let seg = s1 - s0;
    if seg.length_squared() > EPS {
        let n = seg.perp().normalize_or_zero();
        for side in [radius, -radius] {
            let a = s0 + n * side;
            let b = s1 + n * side;
            if let Some((t, _)) = ray_vs_seg(origin, dir, a, b) {
                keep(t);
            }
        }
    }
    best
}

/// Earliest t>=0 where origin + dir*t hits the circle (center c, radius r). None if it misses.
fn ray_vs_circle(origin: Vector2, dir: Vector2, c: Vector2, r: f32) -> Option<f32> {
    let m = origin - c;
    let a = dir.length_squared();
    if a < EPS {
        return None;
    }
    let b = m.dot(dir);
    let cc = m.length_squared() - r * r;
    let disc = b * b - a * cc;
    if disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    if t >= 0.0 {
        Some(t)
    } else {
        let t2 = (-b + disc.sqrt()) / a;
        (t2 >= 0.0).then_some(t2)
    }
}

/// Ray (origin, dir) vs segment [a,b]. Returns (t along ray, u along segment) at the crossing.
fn ray_vs_seg(origin: Vector2, dir: Vector2, a: Vector2, b: Vector2) -> Option<(f32, f32)> {
    let e = b - a;
    let denom = dir.perp_dot(e);
    if denom.abs() < EPS {
        return None; // parallel
    }
    let diff = a - origin;
    let t = diff.perp_dot(e) / denom;
    let u = diff.perp_dot(dir) / denom;
    if t >= 0.0 && (0.0..=1.0).contains(&u) {
        Some((t, u))
    } else {
        None
    }
}

/// Is point p inside the closed polygon `verts` (winding-agnostic, ray-cast parity test)?
pub fn point_in_poly(p: Vector2, verts: &[Vector2]) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (vi, vj) = (verts[i], verts[j]);
        if (vi.y > p.y) != (vj.y > p.y) {
            let x = vi.x + (p.y - vi.y) / (vj.y - vi.y) * (vj.x - vi.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Reflect velocity `v` about surface normal `n` (unit) with restitution `e` (0 = stop, 1 = elastic).
/// `v' = v - (1 + e)(v·n)n`. The wall-bounce / dead-stop primitive.
pub fn reflect(v: Vector2, n: Vector2, e: f32) -> Vector2 {
    v - n * ((1.0 + e) * v.dot(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ball(p: Vector2, r: f32) -> (Iso, Shape) {
        (Iso::at(p), Shape::Ball { r })
    }
    fn seg(a: Vector2, b: Vector2) -> (Iso, Shape) {
        (Iso::at(Vector2::ZERO), Shape::Segment { a, b })
    }

    #[test]
    fn balls_touch_at_sum_of_radii() {
        let g = NaiveGeom;
        let a = ball(Vector2::new(0.0, 0.0), 10.0);
        let b = ball(Vector2::new(25.0, 0.0), 10.0);
        let c = g.contact(a, b, f32::MAX).unwrap();
        assert!((c.dist - 5.0).abs() < 1e-3, "dist {}", c.dist);
        assert!((c.normal1 - Vector2::new(1.0, 0.0)).length() < 1e-3);
        assert!(!g.intersection_test(a, b));
        let b2 = ball(Vector2::new(15.0, 0.0), 10.0);
        assert!(g.intersection_test(a, b2)); // overlap by 5
    }

    #[test]
    fn ball_vs_segment_distance() {
        let g = NaiveGeom;
        let b = ball(Vector2::new(0.0, 0.0), 5.0);
        let s = seg(Vector2::new(-100.0, 20.0), Vector2::new(100.0, 20.0));
        // center is 20 above the line, radius 5 -> 15 separation, normal points up toward the ball.
        let c = g.contact(b, s, f32::MAX).unwrap();
        assert!((c.dist - 15.0).abs() < 1e-3, "dist {}", c.dist);
    }

    #[test]
    fn capsules_use_core_distance() {
        let g = NaiveGeom;
        let a = (Iso::at(Vector2::ZERO), Shape::Capsule { a: Vector2::new(0.0, -30.0), b: Vector2::new(0.0, 30.0), r: 8.0 });
        let b = (Iso::at(Vector2::new(40.0, 0.0)), Shape::Capsule { a: Vector2::new(0.0, -30.0), b: Vector2::new(0.0, 30.0), r: 8.0 });
        // cores are 40 apart, minus 16 of radius -> 24.
        assert!((g.distance(a, b) - 24.0).abs() < 1e-3);
    }

    #[test]
    fn swept_ball_lands_on_platform() {
        let g = NaiveGeom;
        // feet ball at y=0 falling +y, platform top segment at y=100. radius 6 -> touch at y=94.
        let feet = ball(Vector2::new(0.0, 0.0), 6.0);
        let top = seg(Vector2::new(-200.0, 100.0), Vector2::new(200.0, 100.0));
        let hit = g.cast_shapes(feet, Vector2::new(0.0, 100.0), top, Vector2::ZERO, 1.0).unwrap();
        // travels 100/frame; reaches contact (94 of center travel) at t≈0.94.
        assert!((hit.time_of_impact - 0.94).abs() < 1e-2, "toi {}", hit.time_of_impact);
        assert!(hit.normal1.y < 0.0, "normal should point up out of the platform");
    }

    #[test]
    fn swept_miss_returns_none() {
        let g = NaiveGeom;
        let feet = ball(Vector2::new(0.0, 0.0), 6.0);
        let top = seg(Vector2::new(-200.0, 100.0), Vector2::new(200.0, 100.0));
        // moving sideways, never descends to the platform.
        assert!(g.cast_shapes(feet, Vector2::new(100.0, 0.0), top, Vector2::ZERO, 1.0).is_none());
    }

    #[test]
    fn reflect_stop_and_bounce() {
        let n = Vector2::new(0.0, -1.0); // floor normal (up)
        let v = Vector2::new(30.0, 200.0); // moving down-right into the floor
        let stop = reflect(v, n, 0.0);
        assert!((stop.y - 0.0).abs() < 1e-3 && (stop.x - 30.0).abs() < 1e-3); // y killed, x kept
        let bounce = reflect(v, n, 1.0);
        assert!((bounce.y + 200.0).abs() < 1e-3); // fully inverted
    }

    #[test]
    fn point_in_poly_diamond() {
        let d = [
            Vector2::new(0.0, -10.0),
            Vector2::new(10.0, 0.0),
            Vector2::new(0.0, 10.0),
            Vector2::new(-10.0, 0.0),
        ];
        assert!(point_in_poly(Vector2::new(0.0, 0.0), &d));
        assert!(!point_in_poly(Vector2::new(9.0, 9.0), &d));
    }
}
