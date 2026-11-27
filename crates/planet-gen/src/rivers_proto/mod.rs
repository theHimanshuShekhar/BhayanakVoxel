//! PROTOTYPE (#206), THROWAWAY, never merged: two ways to give the 320 km Planet's world map
//! rivers and lakes, side by side on the same C2 map and seed, for the owner to pick one.
//!
//! Both start from the same pass at map build ([`build`]): a priority-flood of the whole cube
//! graph (Barnes, with an epsilon) fills every closed basin to its spill height, which gives each
//! land texel a receiver among its 8 neighbours, the drainage area through it, and the **lake
//! level** of every basin deeper than a metre. The water surface of each river texel is then made
//! to fall downhill: no node's is above an upstream node's.
//!
//! * **D8** ([`RiverMode::D8`]): the receiver is the steepest of the 8 (with hash-jittered
//!   lengths against the grid bias), the area is the integer count of texels draining in. A brick
//!   makes each river edge a straight line between the two texels' hash-jittered points.
//! * **MFD** ([`RiverMode::Mfd`]): the area is split among all lower neighbours by slope to the
//!   fourth power (Holmgren), so it is smoother and disperses over open ground; the map keeps the
//!   dominant receiver and the mean flow direction. A brick makes each edge a Hermite spline whose
//!   tangents are the mean flow directions at its ends (so the river is smooth through every
//!   texel and tributaries join along the main stem's direction), with a noise meander.
//!
//! Both are read per point, from the texels around it alone, at every level of detail
//! ([`shape`], [`bounds`]); nothing is stored below the map. The cost of a river to a brick that
//! has none near it is the texel scan of [`bounds`].
//!
//! Not deterministic across machines in places (see `build`), and not for shipping.
#![allow(clippy::all, clippy::disallowed_methods, dead_code)]

mod build;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use brick_store::VOXELS_PER_METRE;

use crate::world_map::{
    WorldMap, proto_index, proto_node, proto_position, proto_resolve, proto_split,
};

pub use build::build;

/// Which river method a Planet draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiverMode {
    Off,
    /// A: D8 receivers, straight jittered lines.
    D8,
    /// B: multiple-flow routing, splines with meanders.
    Mfd,
}

impl RiverMode {
    pub fn parse(s: &str) -> Option<RiverMode> {
        match s {
            "off" | "none" => Some(RiverMode::Off),
            "d8" => Some(RiverMode::D8),
            "mfd" => Some(RiverMode::Mfd),
            _ => None,
        }
    }
}

static DEFAULT_MODE: AtomicU8 = AtomicU8::new(0);

/// The method that the next world map is built with, and that a new Planet draws (the lookdev
/// sets it from `--proto-rivers`).
pub fn set_default_mode(mode: RiverMode) {
    DEFAULT_MODE.store(mode as u8, Ordering::Relaxed);
}

pub fn default_mode() -> RiverMode {
    match DEFAULT_MODE.load(Ordering::Relaxed) {
        1 => RiverMode::D8,
        2 => RiverMode::Mfd,
        _ => RiverMode::Off,
    }
}

/// The 8 neighbours of a texel, in the order that a receiver's 3 bits name them.
pub(crate) const OFFS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (-1, 1),
    (1, -1),
    (-1, -1),
];

/// `recv` bits.
pub const NO_RECEIVER: u8 = 0x80;
pub const HEAD: u8 = 0x08;
pub const LAKE: u8 = 0x10;
/// `water` of a texel with no river or lake.
pub const NO_WATER: i16 = i16::MIN;

/// A river starts where this many texels drain into one (about 25 km2).
pub const RIVER_MIN_AREA: f64 = 500.0;
/// Km2 in a texel of the 320 km Planet's 2048 map, on average.
const TEXEL_KM2: f64 = 0.0512;
/// Half-width cap, metres.
const HW_MAX: f64 = 80.0;
/// Steps of the flow code to the doubling of the area.
pub const FLOW_STEPS: u32 = 12;
/// How steep a bank rises from the water's edge until it meets the Relief.
const BANK_SLOPE: f64 = 0.35;
/// How deep the bank Material goes, metres.
pub const BANK_THICK: f64 = 0.5;
/// Elevation unit of `water`, in metres (the map's 25 cm).
pub const WATER_UNIT: f64 = 0.25;

/// The stored rivers: per texel of the map, face by face, row by row.
pub struct RiverData {
    pub mode: RiverMode,
    /// Receiver `0..8` (index into [`OFFS`]) in the low 3 bits, [`HEAD`], [`LAKE`], [`NO_RECEIVER`].
    pub recv: Vec<u8>,
    /// 12 steps to the doubling of the texels draining through (D8: counted; MFD: shared out).
    pub flow: Vec<u8>,
    /// MFD only: the mean direction of the flow out, in texel coordinates, normalised to 127.
    pub dir: Vec<[i8; 2]>,
    /// The water surface in 25 cm units above sea level of a river or lake texel, else
    /// [`NO_WATER`].
    pub water: Vec<i16>,
    /// The lowest flow code that is a river.
    pub thr: u8,
}

impl RiverData {
    /// Bytes of storage added per texel by the method.
    pub fn bytes_per_texel(&self) -> usize {
        1 + 1 + 2 + if self.mode == RiverMode::Mfd { 2 } else { 0 }
    }

    pub fn is_river(&self, idx: usize) -> bool {
        self.flow[idx] >= self.thr && self.recv[idx] & LAKE == 0 && self.water[idx] != NO_WATER
    }

    pub fn is_lake(&self, idx: usize) -> bool {
        self.recv[idx] & LAKE != 0
    }
}

/// `2^(code / 24)`: the square root of the area that a flow code stands for, in texels.
fn root_area(code: u8) -> f64 {
    use std::sync::OnceLock;
    static T: OnceLock<[f64; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let step = 1.029_302_236_643_492_f64; // 2^(1/24)
        let mut t = [1.0; 256];
        for k in 1..256 {
            t[k] = t[k - 1] * step;
        }
        t
    })[code as usize]
}

/// Half the river's width in metres for a flow code.
pub fn half_width(code: u8) -> f64 {
    (0.5 * (TEXEL_KM2).sqrt() * root_area(code)).min(HW_MAX)
}

/// How deep the water is in the middle of a river of half-width `hw`, metres.
fn depth_of(hw: f64) -> f64 {
    (0.3 + 0.07 * hw).min(6.0)
}

/// How far below its texel's ground the water surface is, metres.
pub(crate) fn drop_of(hw: f64) -> f64 {
    0.6 * depth_of(hw) + 0.15
}

fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn jitter_of(seed: u64, face: usize, i: i32, j: i32, salt: i32) -> (f64, f64) {
    let h = crate::hash(seed, [face as i32 * 4 + salt, i, j]);
    let f = |bits: u64| (bits >> 11) as f64 / 9_007_199_254_740_992.0 * 2.0 - 1.0;
    (f(h), f(h.rotate_left(29) ^ 0x9e37_79b9_7f4a_7c15))
}

const SEED: u64 = 0x2b99_2ddf_a232_49d6;

/// One river edge, as a polyline of unit directions with what is known along it.
struct Poly {
    pts: Vec<[f64; 3]>,
    /// Half-width, water surface (m above sea) at the two ends.
    hw: [f64; 2],
    w: [f64; 2],
    head: bool,
    hw_max: f64,
}

/// The rivers and lakes that can reach a point of one texel.
struct Cell {
    polys: Vec<Poly>,
    /// Lake texels' centres (unit direction) and levels (m above sea).
    lakes: Vec<([f64; 3], f64)>,
}

thread_local! {
    static CELLS: RefCell<[Option<(usize, usize, Rc<Cell>)>; 4]> = const { RefCell::new([None, None, None, None]) };
    static NEXT: RefCell<usize> = const { RefCell::new(0) };
}

fn texel_chord(size: u32) -> f64 {
    std::f64::consts::FRAC_PI_2 / f64::from(size)
}

/// Where a texel's river node is: its centre moved by a hash, within `reach` of a texel.
fn node_dir(rd: &RiverData, size: u32, face: usize, i: i32, j: i32) -> [f64; 3] {
    let reach = if rd.mode == RiverMode::D8 { 0.32 } else { 0.2 };
    let (jx, jy) = jitter_of(SEED, face, i, j, 0);
    proto_node(size, face, f64::from(i) + jx * reach, f64::from(j) + jy * reach)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { mul(a, 1.0 / l) } else { a }
}

/// The unit direction along which the texel's `i` grows and `j` grows, at a texel.
fn axes(size: u32, face: usize, i: i32, j: i32) -> ([f64; 3], [f64; 3]) {
    let (fi, fj) = (f64::from(i), f64::from(j));
    let u = norm(sub(proto_node(size, face, fi + 0.5, fj), proto_node(size, face, fi - 0.5, fj)));
    let v = norm(sub(proto_node(size, face, fi, fj + 0.5), proto_node(size, face, fi, fj - 0.5)));
    (u, v)
}

const SPLINE_PARTS: usize = 6;

fn build_cell(map: &WorldMap, rd: &RiverData, face: usize, i: i32, j: i32) -> Cell {
    let size = map.size();
    let mut polys = Vec::new();
    let mut lakes = Vec::new();
    for dj in -2..=2 {
        for di in -2..=2 {
            let (f, ti, tj) = proto_resolve(size, face, i + di, j + dj);
            let idx = proto_index(size, f, ti as u32, tj as u32);
            let recv = rd.recv[idx];
            if rd.is_lake(idx) {
                if di.abs() <= 1 && dj.abs() <= 1 {
                    lakes.push((
                        proto_node(size, f, f64::from(ti), f64::from(tj)),
                        f64::from(rd.water[idx]) * WATER_UNIT,
                    ));
                }
            }
            if rd.flow[idx] < rd.thr || recv & NO_RECEIVER != 0 || rd.water[idx] == NO_WATER {
                continue;
            }
            let k = (recv & 7) as usize;
            let (rf, ri, rj) = proto_resolve(size, f, ti + OFFS[k].0, tj + OFFS[k].1);
            let ridx = proto_index(size, rf, ri as u32, rj as u32);
            let lake_here = rd.is_lake(idx);
            if lake_here && rd.is_lake(ridx) {
                continue;
            }
            let a = node_dir(rd, size, f, ti, tj);
            let ocean = rd.water[ridx] == NO_WATER && map.texel(rf, ri as u32, rj as u32).is_ocean();
            let b = node_dir(rd, size, rf, ri, rj);
            let hw_a = half_width(rd.flow[idx]);
            // The end: the receiver's own width where it is a river or lake, the mouth's flare
            // at the sea, else this one's.
            let (hw_b, w_b) = if ocean {
                (hw_a * 1.3, 0.0)
            } else if rd.water[ridx] != NO_WATER {
                (
                    half_width(rd.flow[ridx].max(rd.flow[idx].min(rd.thr))).max(hw_a * 0.9),
                    f64::from(rd.water[ridx]) * WATER_UNIT,
                )
            } else {
                (hw_a, f64::from(rd.water[idx]) * WATER_UNIT - 0.1)
            };
            let w_a = f64::from(rd.water[idx]) * WATER_UNIT;
            let w_b = w_b.min(w_a);
            let head = recv & HEAD != 0;
            let pts = if rd.mode == RiverMode::D8 {
                vec![a, b]
            } else {
                spline(size, rd, (f, ti, tj), (rf, ri, rj), a, b, hw_a)
            };
            polys.push(Poly {
                pts,
                hw: [hw_a, hw_b],
                w: [w_a, w_b],
                head,
                hw_max: hw_a.max(hw_b) * if head { 1.7 } else { 1.0 },
            });
        }
    }
    Cell { polys, lakes }
}

/// B's channel from node `a` to node `b`: a Hermite curve with the flow directions at the two
/// ends for tangents, and a meander that is zero at the ends.
fn spline(
    size: u32,
    rd: &RiverData,
    from: (usize, i32, i32),
    to: (usize, i32, i32),
    a: [f64; 3],
    b: [f64; 3],
    hw: f64,
) -> Vec<[f64; 3]> {
    let chord = sub(b, a);
    let len = dot(chord, chord).sqrt();
    let chord_dir = norm(chord);
    let tangent = |(f, i, j): (usize, i32, i32)| {
        let idx = proto_index(size, f, i as u32, j as u32);
        let [vx, vy] = rd.dir[idx];
        if vx == 0 && vy == 0 {
            return chord_dir;
        }
        let (u, v) = axes(size, f, i, j);
        let m = norm(add(mul(u, f64::from(vx)), mul(v, f64::from(vy))));
        let m = norm(add(mul(chord_dir, 0.4), mul(m, 0.6)));
        if dot(m, chord_dir) < 0.2 { chord_dir } else { m }
    };
    let (ma, mb) = (tangent(from), tangent(to));
    // The meander's amplitude and shape, from the edge's own hash.
    let (h1, h2) = jitter_of(SEED, from.0, from.1, from.2, 1);
    let metres_per_unit = f64::from(crate::world_map::WORLD_RADIUS) / VOXELS_PER_METRE;
    let len_m = len * metres_per_unit;
    let amp = (0.2 * len_m).min(6.0 * hw + 3.0) / metres_per_unit;
    let mid = norm(add(a, b));
    let side = norm(cross(mid, chord_dir));
    (0..=SPLINE_PARTS)
        .map(|k| {
            let t = k as f64 / SPLINE_PARTS as f64;
            let (t2, t3) = (t * t, t * t * t);
            let (h00, h10, h01, h11) = (
                2.0 * t3 - 3.0 * t2 + 1.0,
                t3 - 2.0 * t2 + t,
                -2.0 * t3 + 3.0 * t2,
                t3 - t2,
            );
            let p = add(
                add(mul(a, h00), mul(ma, h10 * len)),
                add(mul(b, h01), mul(mb, h11 * len)),
            );
            let bump = 16.0 * t2 * (1.0 - t) * (1.0 - t);
            let s_bump = bump * (2.0 * t - 1.0) * 2.0;
            let offset = amp * (h1 * bump + 0.6 * h2 * s_bump);
            norm(add(p, mul(side, offset)))
        })
        .collect()
}

fn cell_for(map: &WorldMap, rd: &RiverData, face: usize, i: i32, j: i32) -> Rc<Cell> {
    let key = (rd as *const RiverData as usize, proto_index(map.size(), face, i as u32, j as u32));
    CELLS.with(|cells| {
        let mut cells = cells.borrow_mut();
        for slot in cells.iter().flatten() {
            if (slot.0, slot.1) == key {
                return slot.2.clone();
            }
        }
        let cell = Rc::new(build_cell(map, rd, face, i, j));
        let n = NEXT.with(|n| {
            let mut n = n.borrow_mut();
            *n = (*n + 1) % 4;
            *n
        });
        cells[n] = Some((key.0, key.1, cell.clone()));
        cell
    })
}

/// What a point has of the water, for the Material there.
#[derive(Clone, Copy, Debug)]
pub struct Wet {
    /// The water surface in Voxels above sea level, or `f64::NEG_INFINITY`.
    pub water: f64,
    /// Whether the ground here has the bank Material in its top [`BANK_THICK`].
    pub bank: bool,
}

impl Wet {
    pub const NONE: Wet = Wet {
        water: f64::NEG_INFINITY,
        bank: false,
    };
}

/// The ground's offset from the map's elevation at `point` (`radius` Voxels from the centre),
/// where the detail is `detail` Voxels and the elevation `elevation`, for a brick sampled `size`
/// Voxels apart: the detail damped near a river, the channel and its banks cut through it, and
/// the water. All in Voxels.
pub(crate) fn shape(
    map: &WorldMap,
    rd: &RiverData,
    point: [f64; 3],
    radius: f64,
    elevation: f64,
    detail: f64,
    size: f64,
) -> (f64, Wet) {
    let n = map.size() as i32;
    let dir = point.map(|c| c / radius);
    let (face, x, y) = proto_position(map.size(), dir);
    let (i, j) = ((x as i32).clamp(0, n - 1), (y as i32).clamp(0, n - 1));
    let cell = cell_for(map, rd, face, i, j);
    if cell.polys.is_empty() && cell.lakes.is_empty() {
        return (detail, Wet::NONE);
    }
    let vpm = VOXELS_PER_METRE;
    let size_m = size / vpm;
    let (elev_m, detail_m) = (elevation / vpm, detail / vpm);
    let mut channel = f64::INFINITY;
    let mut mask: f64 = 0.0;
    let mut water = f64::NEG_INFINITY;
    let mut bank_zone = false;
    let mut lip: f64 = f64::NEG_INFINITY;
    for poly in &cell.polys {
        if poly.hw_max < 0.4 * size_m {
            continue;
        }
        let mut best = (f64::INFINITY, 0.0);
        let parts = poly.pts.len() - 1;
        for (k, seg) in poly.pts.windows(2).enumerate() {
            let d = sub(seg[1], seg[0]);
            let u = (dot(sub(dir, seg[0]), d) / dot(d, d)).clamp(0.0, 1.0);
            let c = add(seg[0], mul(d, u));
            let e = sub(dir, c);
            let dist2 = dot(e, e);
            if dist2 < best.0 {
                best = (dist2, (k as f64 + u) / parts as f64);
            }
        }
        let s = best.0.sqrt() * radius / vpm;
        let t = best.1;
        let flare = if poly.head { 1.0 + 0.7 * (1.0 - t / 0.35).max(0.0) } else { 1.0 };
        let hw = (poly.hw[0] + (poly.hw[1] - poly.hw[0]) * t) * flare;
        if s > 6.0 * hw + 40.0 {
            continue;
        }
        let w = poly.w[0] + (poly.w[1] - poly.w[0]) * t;
        let hd = depth_of(hw);
        mask = mask.max(1.0 - smoothstep(2.0 * hw + 3.0, 6.0 * hw + 40.0, s));
        let fl = if s < hw {
            let q = s / hw;
            water = water.max(w);
            w - hd * (1.0 - q * q)
        } else {
            w + BANK_SLOPE * (s - hw)
        };
        channel = channel.min(fl);
        if s < hw + 1.5 + 0.6 * hw {
            bank_zone = true;
        }
        if s >= hw && s < 5.0 * hw + 4.0 && w > 0.3 && elev_m + detail_m * (1.0 - mask) > w - 2.0 {
            lip = lip.max(w + 0.15);
        }
    }
    let mut ground = elev_m + detail_m * (1.0 - mask);
    if channel.is_finite() {
        ground = ground.min(channel);
        if lip.is_finite() {
            // Where the Relief dips under the water beside the channel, the bank holds it.
            ground = ground.max(lip.min(channel));
        }
    }
    // Lakes: the nearest lake texel within a texel and a quarter sets the level.
    let mut wet_water = water;
    let mut lake_bank = false;
    let reach = 1.25 * texel_chord(map.size());
    let mut near: Option<(f64, f64)> = None;
    for &(c, level) in &cell.lakes {
        let e = sub(dir, c);
        let d = dot(e, e).sqrt();
        if d < reach && near.is_none_or(|b| d < b.0) {
            near = Some((d, level));
        }
    }
    if let Some((_, level)) = near {
        if level > wet_water {
            wet_water = level;
        }
        if ground < level + 1.0 {
            lake_bank = true;
        }
    }
    (
        ground * vpm - elevation,
        Wet {
            water: if wet_water.is_finite() { wet_water * vpm } else { f64::NEG_INFINITY },
            bank: bank_zone || lake_bank,
        },
    )
}

/// Bounds on what the rivers do to the ground over the box of `corners` (unit-less points of
/// the brick, any radius), for Voxels `size` apart: the lowest the bed can be and the highest the
/// water can be, in Voxels above sea level; `None` where no river or lake is near, or the box is
/// too wide to look at (beyond 24 texels, the coarsest bricks).
pub(crate) fn bounds(
    map: &WorldMap,
    rd: &RiverData,
    corners: &[[f64; 3]],
    size: f64,
) -> Option<(f64, f64)> {
    let n = map.size() as i32;
    let vpm = VOXELS_PER_METRE;
    let size_m = size / vpm;
    // The texel box of each face the corners are on.
    let mut boxes: Vec<(usize, i32, i32, i32, i32)> = Vec::new();
    for &c in corners {
        let (f, x, y) = proto_position(map.size(), c);
        let (i, j) = ((x as i32).clamp(0, n - 1), (y as i32).clamp(0, n - 1));
        match boxes.iter_mut().find(|b| b.0 == f) {
            Some(b) => {
                b.1 = b.1.min(i);
                b.2 = b.2.max(i);
                b.3 = b.3.min(j);
                b.4 = b.4.max(j);
            }
            None => boxes.push((f, i, i, j, j)),
        }
    }
    let mut low = f64::INFINITY;
    let mut top = f64::NEG_INFINITY;
    for &(face, i0, i1, j0, j1) in &boxes {
        if i1 - i0 > 24 || j1 - j0 > 24 {
            return None;
        }
        for j in j0 - 2..=j1 + 2 {
            for i in i0 - 2..=i1 + 2 {
                let (f, ti, tj) = proto_resolve(map.size(), face, i, j);
                let idx = proto_index(map.size(), f, ti as u32, tj as u32);
                if rd.water[idx] == NO_WATER {
                    continue;
                }
                let w = f64::from(rd.water[idx]) * WATER_UNIT;
                if rd.is_lake(idx) {
                    top = top.max(w + 0.3);
                    continue;
                }
                if rd.flow[idx] < rd.thr {
                    continue;
                }
                let hw = half_width(rd.flow[idx]) * 1.7;
                if hw < 0.4 * size_m {
                    continue;
                }
                low = low.min(w - depth_of(hw) - 1.0);
                top = top.max(w + 0.6);
            }
        }
    }
    (top.is_finite() || low.is_finite()).then(|| {
        (
            if low.is_finite() { low * vpm } else { f64::INFINITY },
            if top.is_finite() { top * vpm } else { f64::NEG_INFINITY },
        )
    })
}

/// Stats for the dev-box images and the report: for each river texel, in map order, a line.
pub fn node_at(map: &WorldMap, face: usize, i: u32, j: u32) -> Option<(u8, bool, i16)> {
    let rd = map.rivers()?;
    let idx = proto_index(map.size(), face, i, j);
    Some((rd.flow[idx], rd.is_lake(idx), rd.water[idx]))
}

pub fn texel_index(map: &WorldMap, idx: usize) -> (usize, u32, u32) {
    proto_split(map.size(), idx)
}

/// The unit direction of the river node of texel `(i, j)` of `face`.
pub fn node_point(map: &WorldMap, face: usize, i: u32, j: u32) -> [f64; 3] {
    match map.rivers() {
        Some(rd) => node_dir(rd, map.size(), face, i as i32, j as i32),
        None => map.texel_direction(face, i, j),
    }
}

/// The texel that texel `(i, j)` of `face` drains into, if it has a receiver.
pub fn receiver_of(map: &WorldMap, face: usize, i: u32, j: u32) -> Option<(usize, u32, u32)> {
    let rd = map.rivers()?;
    let idx = proto_index(map.size(), face, i, j);
    if rd.recv[idx] & NO_RECEIVER != 0 {
        return None;
    }
    let k = (rd.recv[idx] & 7) as usize;
    let (f, a, b) = proto_resolve(map.size(), face, i as i32 + OFFS[k].0, j as i32 + OFFS[k].1);
    Some((f, a as u32, b as u32))
}
