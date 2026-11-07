//! What a path asks for: per frame, which bricks are required, which the shell holds and which
//! prefetch adds, as the changes from the frame before.
//!
//! The demand depends on the path and the model, not on how fast anything is generated, so it is
//! worked out once per path and replayed against any number of rates ([`crate::cache`]).
//!
//! **The required set** ([`VISIBLE`]) is the Visible set of the glossary, approximated: the bricks
//! the band rule picks for the camera (the traversal below is the loader's: a brick is split into
//! its eight children while it is nearer than its level's boundary, [`Bands::split`]), that are
//! inside the view frustum (a bounding sphere against the four side planes) and inside the horizon
//! cap (nearer than the horizon). Occlusion is not modelled, so the set is a superset of the real
//! one, which makes holes and misses pessimistic.
//!
//! **The shell** ([`SHELL`]) is the same traversal in every direction, out to the radial margin
//! times the horizon, and again for a camera one vertical margin higher. **Prefetch**
//! ([`PREFETCH`]) is the shell again at the camera's predicted position (see
//! [`crate::model::Model::lead`]), which is what a path is going to need next.
//!
//! The surface is the sphere (with the model's relief): a brick is in a set only if it meets it.
//! Bricks deep in the strata, which nothing can see until a dig exposes them, are not.

use brick_store::BrickCoord;
use render::lod::brick_distance;

use crate::key::{Key, KeyMap};
use crate::model::{Model, add, dot, norm, scale, sub, unit};
use crate::path::{Path, Sample};

/// A brick the frame needs: in the view, inside the horizon.
pub const VISIBLE: u8 = 1;
/// A brick of the 360° shell, wherever the camera looks.
pub const SHELL: u8 = 2;
/// A brick of the shell at where the camera is going.
pub const PREFETCH: u8 = 4;

/// Places along its velocity that prefetch builds the shell at, up to the end of the lead.
pub const LEAD_POINTS: usize = 4;

/// The camera positions that a frame's shell is built for: the camera and one vertical margin
/// above it, then each prefetch position and one margin above that. Empty slots reach nowhere.
pub const SLOTS: usize = 16;

/// A camera position that the shell is built for, and how far it reaches.
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub pos: [f64; 3],
    pub reach: f64,
}

impl Point {
    /// A slot with no position: nothing is nearer than its reach.
    pub const NONE: Point = Point {
        pos: [0.0; 3],
        reach: -1.0,
    };
}

/// What a point does with a brick that it reaches.
#[derive(PartialEq, Eq)]
enum Decision {
    /// Farther than the reach, and so is everything under it.
    Beyond,
    /// Nearer than its level is used from: the eight below it are picked instead.
    Split,
    Pick,
}

fn decide(model: &Model, point: &Point, level: usize, coord: BrickCoord) -> (Decision, f64) {
    let distance = brick_distance(point.pos, level, coord);
    let decision = if distance > point.reach {
        Decision::Beyond
    } else if model.bands.split(level, distance, None) {
        Decision::Split
    } else {
        Decision::Pick
    };
    (decision, distance)
}

/// The roots: the bricks of the top level that can hold the surface.
fn roots(model: &Model) -> impl Iterator<Item = BrickCoord> {
    let size = (8u64 << model.top()) as f64;
    let reach = model.planet_radius + model.relief + model.crust * size;
    let (low, high) = (
        (-reach / size).floor() as i32,
        (reach / size).floor() as i32,
    );
    (low..=high).flat_map(move |x| {
        (low..=high).flat_map(move |y| (low..=high).map(move |z| BrickCoord([x, y, z])))
    })
}

/// Visits the bricks that the band rule picks for each of `points`, nearer than the point's reach:
/// calls `emit` with the level, the coordinate and a mask of the points (bit `i` for the `i`th)
/// that picked it. A brick can be picked at different levels by different points.
pub fn traverse(model: &Model, points: &[Point], mut emit: impl FnMut(usize, BrickCoord, u16)) {
    let all = ((1u32 << points.len()) - 1) as u16;
    for root in roots(model) {
        visit(model, points, model.top(), root, all, &mut emit);
    }
}

fn visit(
    model: &Model,
    points: &[Point],
    level: usize,
    coord: BrickCoord,
    active: u16,
    emit: &mut impl FnMut(usize, BrickCoord, u16),
) {
    if !model.is_surface(level, coord.0) {
        return;
    }
    let (mut picked, mut split) = (0u16, 0u16);
    for (i, point) in points.iter().enumerate() {
        if active >> i & 1 == 0 {
            continue;
        }
        match decide(model, point, level, coord).0 {
            Decision::Beyond => {}
            Decision::Split => split |= 1 << i,
            Decision::Pick => picked |= 1 << i,
        }
    }
    if picked != 0 {
        emit(level, coord, picked);
    }
    if split != 0 {
        for child in coord.children() {
            visit(model, points, level - 1, child, split, emit);
        }
    }
}

/// What [`traverse`] would change going from `old` to `new` (the same number of points each):
/// calls `emit` with the level, the coordinate, and the masks of the points that picked the brick
/// before and after, for every brick where they differ.
///
/// It does not visit all the bricks, which a camera that moves a few metres does not change. A
/// brick's pick depends on its distance against a few thresholds (where each level starts, and
/// each point's reach), and a distance moves no further than the camera, so a brick whose box
/// is not within the camera's move of any threshold, for the points that reach it, picks as it did,
/// and so do all the bricks under it. Only the bricks near the thresholds are visited: a thin
/// shell of them for each level.
pub fn diff_traverse(
    model: &Model,
    old: &[Point],
    new: &[Point],
    mut emit: impl FnMut(usize, BrickCoord, u16, u16),
) {
    assert_eq!(old.len(), new.len());
    let all = ((1u32 << old.len()) - 1) as u16;
    let floors: Vec<f64> = (1..=model.top()).map(|l| model.bands.floor(l)).collect();
    let used = (0..old.len())
        .filter(|&i| old[i].reach >= 0.0 || new[i].reach >= 0.0)
        .collect();
    let tree = Diff {
        model,
        old,
        new,
        floors,
        used,
    };
    for root in roots(model) {
        tree.visit(model.top(), root, all, all, &mut emit);
    }
}

struct Diff<'a> {
    model: &'a Model,
    old: &'a [Point],
    new: &'a [Point],
    /// Where each level above 0 starts.
    floors: Vec<f64>,
    /// The points that reach anywhere, before or after.
    used: Vec<usize>,
}

impl Diff<'_> {
    fn visit(
        &self,
        level: usize,
        coord: BrickCoord,
        active_old: u16,
        active_new: u16,
        emit: &mut impl FnMut(usize, BrickCoord, u16, u16),
    ) {
        if !self.model.is_surface(level, coord.0) {
            return;
        }
        let diagonal = (8u64 << level) as f64 * 3f64.sqrt();
        let (mut picked_old, mut split_old, mut picked_new, mut split_new) =
            (0u16, 0u16, 0u16, 0u16);
        // Whether some point that splits it, as it did, has a threshold within reach of a brick
        // under this one.
        let mut near = false;
        for &i in &self.used {
            let (mut before, mut after) = (f64::NAN, f64::NAN);
            if active_old >> i & 1 == 1 {
                let (decision, d) = decide(self.model, &self.old[i], level, coord);
                before = d;
                match decision {
                    Decision::Beyond => {}
                    Decision::Split => split_old |= 1 << i,
                    Decision::Pick => picked_old |= 1 << i,
                }
            }
            if active_new >> i & 1 == 1 {
                let (decision, d) = decide(self.model, &self.new[i], level, coord);
                after = d;
                match decision {
                    Decision::Beyond => {}
                    Decision::Split => split_new |= 1 << i,
                    Decision::Pick => picked_new |= 1 << i,
                }
            }
            if !near && (split_old | split_new) >> i & 1 == 1 {
                let (low, high) = (before.min(after), before.max(after) + diagonal);
                let within = |t: f64| t >= low && t <= high;
                near = self.floors.iter().any(|&t| within(t))
                    || within(self.old[i].reach)
                    || within(self.new[i].reach);
            }
        }
        if picked_old != picked_new {
            emit(level, coord, picked_old, picked_new);
        }
        let differs = active_old != active_new || split_old != split_new;
        if (split_old | split_new) != 0 && (differs || near) {
            for child in coord.children() {
                self.visit(level - 1, child, split_old, split_new, emit);
            }
        }
    }
}

/// The view of a frame, for testing bricks against.
struct View {
    pos: [f64; 3],
    fwd: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    /// Half-angle tangents.
    tan: [f64; 2],
    /// How far the ground is seen.
    horizon: f64,
}

impl View {
    fn new(model: &Model, s: &Sample) -> Self {
        Self {
            pos: s.pos,
            fwd: s.fwd,
            right: s.right,
            up: s.up,
            tan: model.tan_half_fov,
            horizon: model.horizon(model.altitude(s.pos)),
        }
    }

    /// Whether the brick could show a pixel: nearer than the horizon, and its bounding sphere not
    /// wholly outside any of the four side planes.
    fn sees(&self, level: usize, coord: BrickCoord) -> bool {
        if brick_distance(self.pos, level, coord) > self.horizon {
            return false;
        }
        let size = (8u64 << level) as f64;
        let centre = coord.0.map(|c| (f64::from(c) + 0.5) * size);
        let radius = size * 0.5 * 3f64.sqrt();
        let v = sub(centre, self.pos);
        let (x, y, z) = (dot(v, self.right), dot(v, self.up), dot(v, self.fwd));
        let [th, tv] = self.tan;
        // A plane through the camera with unit normal n: the sphere is outside if n·v > radius.
        let side = |across: f64, tan: f64| (across.abs() - z * tan) / (1.0 + tan * tan).sqrt();
        side(x, th) <= radius && side(y, tv) <= radius
    }
}

/// One change to a brick's membership: its mask of [`VISIBLE`], [`SHELL`] and [`PREFETCH`] was
/// `old` and is now `new` (0 for in none, so not wanted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub key: Key,
    pub old: u8,
    pub new: u8,
}

/// What one frame changes.
#[derive(Clone, Debug, Default)]
pub struct StepDemand {
    pub pos: [f64; 3],
    pub changes: Vec<Change>,
    /// Bricks of the Guard that the frame needs: always resident, so not tracked one by one.
    pub guard_visible: u32,
}

#[derive(Clone, Debug)]
pub struct Demand {
    pub dt: f64,
    pub steps: Vec<StepDemand>,
    /// The step in which the camera first has a velocity (or a Zoom's look-ahead), after the
    /// standing frame and any samples at rest: the frame at which the shell has to follow a
    /// camera that nothing could have predicted. A standing camera does not know which way it
    /// will go, so the bricks that a first step of a metre or two brings into a finer band
    /// cannot be resident already (a one-frame latency that no prefetch beats). `steps.len()`
    /// if the path never moves.
    pub onset: usize,
}

impl Demand {
    /// The most bricks that the shell wants at once, over the frames (prefetch not counted).
    pub fn peak_shell(&self) -> usize {
        let (mut now, mut peak) = (0i64, 0i64);
        for step in &self.steps {
            for c in &step.changes {
                let wanted = |m: u8| i64::from(m & SHELL != 0);
                now += wanted(c.new) - wanted(c.old);
            }
            peak = peak.max(now);
        }
        peak as usize
    }
}

/// Where prefetch aims for a sample: the positions that the path says it is going through, if
/// it says (a Zoom's controller knows), else one place along the camera's velocity, a lead time
/// ahead, with its altitude kept above the ground.
fn predict(model: &Model, s: &Sample) -> Vec<[f64; 3]> {
    if !s.ahead.is_empty() {
        return s.ahead.clone();
    }
    let Some((min, max)) = model.lead else {
        return Vec::new();
    };
    let speed = norm(s.vel);
    if speed < 1.0 {
        return Vec::new();
    }
    let altitude = model.altitude(s.pos);
    let lead = (2.0 * model.horizon(altitude) / speed).clamp(min, max);
    // Along the ground by the tangential part, and up by the radial part, at `LEAD_POINTS` even
    // steps of the lead: the shell has to be there at the places between, where the bands the
    // camera crosses are not those of either end.
    let up = unit(s.pos);
    let radial = dot(s.vel, up);
    let flat = sub(s.vel, scale(up, radial));
    (1..=LEAD_POINTS)
        .map(|k| {
            let t = lead * k as f64 / LEAD_POINTS as f64;
            let direction = unit(add(s.pos, scale(flat, t)));
            let height = (altitude + radial * t).max(crate::model::EYE);
            scale(direction, model.planet_radius + height)
        })
        .collect()
}

/// The points that the shell is built for at a sample: the camera and one vertical margin above
/// it, then each place prefetch aims at (and, for the last, one margin above it). Which mask bit
/// each carries is [`class`].
fn points(model: &Model, s: &Sample) -> [Point; SLOTS] {
    let mut out = Vec::with_capacity(SLOTS);
    let mut push = |pos: [f64; 3], lift: f64| {
        let raised = add(pos, scale(unit(pos), lift));
        out.push(Point {
            pos: raised,
            reach: model.shell_reach(model.altitude(raised)),
        });
    };
    push(s.pos, 0.0);
    push(s.pos, model.vertical_margin);
    // A controller's places are every octave of the way: they have no margin but the last one's.
    // One extrapolated place is the whole of the guess, and has it.
    let ahead = predict(model, s);
    for (i, &pos) in ahead.iter().enumerate() {
        push(pos, 0.0);
        if i + 1 == ahead.len() {
            push(pos, model.vertical_margin);
        }
    }
    let mut slots = [Point::NONE; SLOTS];
    for (slot, point) in slots.iter_mut().zip(out) {
        *slot = point;
    }
    slots
}

/// A brick's mask from which points picked it, and whether the camera sees it.
fn class(picked: u16, visible: bool) -> u8 {
    let mut mask = 0;
    if picked & 0b11 != 0 {
        mask |= SHELL;
    }
    if picked & 0b1 != 0 && visible {
        mask |= VISIBLE;
    }
    if picked & !0b11 != 0 {
        mask |= PREFETCH;
    }
    mask
}

/// A brick of the shell that the demand is tracking: which points pick it, and whether the view
/// sees it.
#[derive(Clone, Copy, Default)]
struct Tracked {
    picked: u16,
    visible: bool,
}

impl Tracked {
    fn class(self) -> u8 {
        class(self.picked, self.visible)
    }
}

/// The demand of every frame of `path`, after a first frame at rest: the camera at the path's
/// start with no velocity and nothing ahead, which is what a camera standing there holds (the
/// shell and the Guard, no prefetch) and so what the cache starts with. The frame after it has
/// the path's own velocity and look-ahead, so that the shell where it is going has to be
/// generated. The first frame is the whole shell, a traversal; each one after is the change
/// from the one before ([`diff_traverse`]), and which of the bricks are in the view, which is
/// worked out for them all.
pub fn build(model: &Model, path: &Path) -> Demand {
    let mut steps = Vec::with_capacity(path.samples.len());
    let mut tracked: KeyMap<Tracked> = KeyMap::default();
    // The Guard's bricks that the camera's own point picks: only their number in view matters.
    let mut guard: KeyMap<u16> = KeyMap::default();
    let mut was = [Point::NONE; SLOTS];
    let rest = path.samples.first().map(|first| Sample {
        vel: [0.0; 3],
        ahead: Vec::new(),
        ..first.clone()
    });
    for s in rest.iter().chain(&path.samples) {
        let now = points(model, s);
        let view = View::new(model, s);
        // The class that each brick that this frame touches had before it.
        let mut before: KeyMap<u8> = KeyMap::default();
        diff_traverse(model, &was, &now, |level, coord, old, new| {
            if model.in_guard(level) {
                // Only the camera's own point matters for the Guard.
                match new & 1 {
                    0 => guard.remove(&Key::new(level, coord)),
                    _ => guard.insert(Key::new(level, coord), new),
                };
                return;
            }
            let key = Key::new(level, coord);
            let entry = tracked.entry(key).or_default();
            before.entry(key).or_insert(entry.class());
            debug_assert_eq!(entry.picked, old);
            entry.picked = new;
            entry.visible = new & 1 != 0 && view.sees(level, coord);
            if new == 0 {
                tracked.remove(&key);
            }
        });
        // The view moves too, so every brick that the camera picks is tested against it.
        for (&key, t) in tracked.iter_mut() {
            if t.picked & 1 != 0 {
                let visible = view.sees(key.level(), key.coord());
                if visible != t.visible {
                    before.entry(key).or_insert(t.class());
                    t.visible = visible;
                }
            }
        }
        let changes = before
            .into_iter()
            .filter_map(|(key, old)| {
                let new = tracked.get(&key).map_or(0, |t| t.class());
                (old != new).then_some(Change { key, old, new })
            })
            .collect();
        let guard_visible = guard
            .keys()
            .filter(|k| view.sees(k.level(), k.coord()))
            .count() as u32;
        steps.push(StepDemand {
            pos: s.pos,
            changes,
            guard_visible,
        });
        was = now;
    }
    let onset = rest
        .iter()
        .chain(&path.samples)
        .position(|s| norm(s.vel) > 1.0 || !s.ahead.is_empty())
        .unwrap_or(steps.len());
    Demand {
        dt: path.dt,
        steps,
        onset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VOXELS_PER_METRE;
    use render::lod::Bands;

    /// A Planet of 800 Voxels (80 m) with the first boundary at 40 Voxels, five levels.
    fn tiny() -> Model {
        let mut model = Model::real();
        model.planet_radius = 800.0;
        model.crust = 0.0;
        model.bands = Bands::with_d0(20.0);
        model.levels = 5;
        model.guard_first = Some(3);
        model.vertical_margin = 40.0;
        model
    }

    fn picked_by_one(model: &Model, pos: [f64; 3], reach: f64) -> Vec<(usize, BrickCoord)> {
        let mut out = Vec::new();
        traverse(model, &[Point { pos, reach }], |l, c, _| out.push((l, c)));
        out
    }

    #[test]
    fn a_path_starts_from_rest_so_its_prefetch_is_a_load_after_the_first_frame() {
        let model = tiny();
        let path = crate::path::takeoff(&model, 5.0, 1.0, 0.25);
        let demand = build(&model, &path);
        assert_eq!(demand.steps.len(), path.samples.len() + 1);
        let prefetch = |step: &StepDemand| {
            step.changes
                .iter()
                .filter(|c| c.new & PREFETCH != 0)
                .count()
        };
        assert_eq!(prefetch(&demand.steps[0]), 0, "standing, nothing is ahead");
        assert_eq!(
            prefetch(&demand.steps[1]),
            0,
            "still at rest on the first sample"
        );
        assert!(
            prefetch(&demand.steps[2]) > 0,
            "the take-off's shell is ahead once it moves"
        );
    }

    #[test]
    fn the_diff_is_what_a_second_traversal_changes() {
        let model = tiny();
        let points = |x: f64| {
            [
                Point {
                    pos: [x, 3.0, 802.0],
                    reach: model.shell_reach(2.0),
                },
                Point {
                    pos: [x, 3.0, 842.0],
                    reach: model.shell_reach(42.0),
                },
            ]
        };
        let full = |points: &[Point]| {
            let mut found = KeyMap::default();
            traverse(&model, points, |l, c, mask| {
                found.insert(Key::new(l, c), mask);
            });
            found
        };
        // No move, a Voxel, a few, a brick or two, and a jump across the Planet's cap.
        for (a, b) in [
            (0.0, 0.0),
            (0.0, 1.0),
            (0.0, 3.0),
            (0.0, 17.0),
            (10.0, -90.0),
        ] {
            let (old, new) = (points(a), points(b));
            let (was, now) = (full(&old), full(&new));
            let mut expected: Vec<(Key, u16, u16)> = was
                .keys()
                .chain(now.keys())
                .filter(|k| was.get(k) != now.get(k))
                .map(|k| {
                    (
                        *k,
                        was.get(k).copied().unwrap_or(0),
                        now.get(k).copied().unwrap_or(0),
                    )
                })
                .collect();
            expected.sort();
            expected.dedup();
            let mut got = Vec::new();
            diff_traverse(&model, &old, &new, |l, c, o, n| {
                got.push((Key::new(l, c), o, n))
            });
            got.sort();
            assert_eq!(got, expected, "{a} to {b}");
            if a == b {
                assert!(got.is_empty());
            }
        }
    }

    #[test]
    fn each_brick_is_at_the_level_whose_band_owns_its_distance() {
        let model = tiny();
        let pos = [0.0, 0.0, 802.0];
        let picked = picked_by_one(&model, pos, 1e9);
        assert!(!picked.is_empty());
        for &(level, coord) in &picked {
            let d = brick_distance(pos, level, coord);
            // A brick is picked once it is not nearer than its level starts, and it was split
            // from a parent that was nearer than the next level starts (at the box's nearest
            // point, so the child's own distance can be a little more).
            assert!(d >= model.bands.floor(level), "{level} {coord:?} at {d}");
            if level < model.top() {
                let parent = brick_distance(pos, level + 1, coord.parent());
                assert!(parent < model.bands.floor(level + 1), "{level} {coord:?}");
            }
        }
        // The camera's own brick is the finest.
        assert!(picked.iter().any(|&(l, _)| l == 0));
    }

    #[test]
    fn the_picked_bricks_cover_every_surface_point_in_reach_exactly_once() {
        let model = tiny();
        let pos = [0.0, 0.0, 802.0];
        let reach = 300.0;
        let picked = picked_by_one(&model, pos, reach);
        let mut checked = 0;
        for i in 0..90 {
            for j in 0..90 {
                let (theta, phi) = (
                    0.35 * f64::from(i) / 90.0,
                    f64::from(j) / 90.0 * std::f64::consts::TAU,
                );
                let on = [
                    800.0 * theta.sin() * phi.cos(),
                    800.0 * theta.sin() * phi.sin(),
                    800.0 * theta.cos(),
                ];
                if norm(sub(on, pos)) > reach {
                    continue;
                }
                checked += 1;
                let holding = picked
                    .iter()
                    .filter(|&&(level, coord)| {
                        let size = (8u64 << level) as f64;
                        (0..3).all(|a| {
                            let low = f64::from(coord.0[a]) * size;
                            (low..low + size).contains(&on[a])
                        })
                    })
                    .count();
                assert_eq!(holding, 1, "{on:?}");
            }
        }
        assert!(checked > 1000);
    }

    #[test]
    fn nothing_is_picked_beyond_the_reach() {
        let model = tiny();
        let pos = [0.0, 0.0, 802.0];
        for (level, coord) in picked_by_one(&model, pos, 150.0) {
            assert!(brick_distance(pos, level, coord) <= 150.0);
        }
        // Fifty Voxels up, nothing is within ten.
        assert!(picked_by_one(&model, [0.0, 0.0, 850.0], 10.0).is_empty());
    }

    #[test]
    fn the_shell_reaches_the_horizon_times_the_margin_in_every_direction() {
        let model = tiny();
        let pos = [0.0, 0.0, 802.0];
        let reach = model.shell_reach(2.0);
        let picked = picked_by_one(&model, pos, reach);
        let far = picked
            .iter()
            .map(|&(l, c)| brick_distance(pos, l, c))
            .fold(0.0, f64::max);
        assert!(far > 0.9 * reach && far <= reach, "{far} of {reach}");
        // Complete in azimuth: bricks on both sides of the camera on x, and on y.
        for axis in [0, 1] {
            let sign = |positive: bool| {
                picked.iter().any(|&(l, c)| {
                    let mid = (f64::from(c.0[axis]) + 0.5) * (8u64 << l) as f64;
                    (mid > 0.0) == positive && mid.abs() > 50.0
                })
            };
            assert!(sign(true) && sign(false), "axis {axis}");
        }
    }

    fn view_from(model: &Model, pos: [f64; 3], fwd: [f64; 3]) -> View {
        let up = unit(pos);
        let right = crate::model::cross(fwd, up);
        let s = Sample {
            pos,
            fwd,
            right,
            up: crate::model::cross(right, fwd),
            vel: [0.0; 3],
            ahead: Vec::new(),
        };
        View::new(model, &s)
    }

    #[test]
    fn the_view_sees_ahead_and_not_behind_and_stops_at_the_horizon() {
        let model = tiny();
        let view = view_from(&model, [0.0, 0.0, 802.0], [1.0, 0.0, 0.0]);
        // A level 0 brick 20 Voxels ahead and one 20 behind, on the ground (z about 800).
        assert!(view.sees(0, BrickCoord([2, 0, 99])));
        assert!(!view.sees(0, BrickCoord([-3, 0, 99])));
        // A brick on the horizon's far side is beyond it: the horizon at 2 Voxels up is 56.6.
        assert!(!view.sees(0, BrickCoord([12, 0, 99])));
        // The view is 102 degrees across: 40 to the side and 10 ahead is outside, 10 and 10 in.
        assert!(!view.sees(0, BrickCoord([1, 5, 99])));
        assert!(view.sees(0, BrickCoord([1, 1, 99])));
    }

    #[test]
    fn a_turn_changes_what_is_visible_and_not_what_the_shell_holds() {
        let model = tiny();
        let path = crate::path::turn(&model, 180.0, 1.0, 0.25);
        let demand = build(&model, &path);
        let shell = |step: &StepDemand| {
            step.changes
                .iter()
                .filter(|c| (c.new ^ c.old) & SHELL != 0)
                .count()
        };
        assert!(shell(&demand.steps[0]) > 0);
        for step in &demand.steps[2..] {
            assert_eq!(shell(step), 0, "a turn does not move the shell");
            assert!(!step.changes.is_empty(), "but the view moves");
        }
    }

    #[test]
    fn flying_gains_bricks_ahead_and_drops_them_behind() {
        let mut model = tiny();
        model.lead = None;
        // 50 m/s is 500 Voxels/s: 125 Voxels in a quarter second.
        let path = crate::path::flight(&model, 0.2, 50.0 / VOXELS_PER_METRE * 10.0, 2.0, 0.25);
        let demand = build(&model, &path);
        let step = &demand.steps[6];
        let gained = step.changes.iter().filter(|c| c.old == 0).count();
        let lost = step.changes.iter().filter(|c| c.new == 0).count();
        assert!(gained > 0 && lost > 0);
    }

    #[test]
    fn prefetch_is_the_shell_at_the_predicted_position() {
        let mut model = tiny();
        model.guard_first = None;
        let mut path = crate::path::flight(&model, 0.2, 5.0, 2.0, 0.25);
        path.samples.drain(..5);
        path.samples.truncate(1);
        let with = build(&model, &path);
        model.lead = None;
        let without = build(&model, &path);
        // The rest frame, then the first frame of the path, which has a velocity.
        let count = |d: &Demand, bit: u8| {
            d.steps[..2]
                .iter()
                .flat_map(|step| &step.changes)
                .filter(|c| c.new & bit != 0 && c.old & bit == 0)
                .count()
        };
        assert_eq!(count(&without, PREFETCH), 0);
        assert!(count(&with, PREFETCH) > 0);
        // Same shell either way.
        assert_eq!(count(&with, SHELL), count(&without, SHELL));
    }
}
