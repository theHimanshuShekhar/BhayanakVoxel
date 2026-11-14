//! The LOD band rule: which level of the brick pyramid (ADR 0002) a brick is drawn at, from its
//! distance to the camera.
//!
//! A level `L` Voxel is `2^L` Voxels of level 0 across, and should cover at most one *output*
//! pixel. A level 0 Voxel covers one output pixel at `d0` from the camera, so level `L` is used
//! from `2^L · d0` on, until a Voxel of level `L + 1` covers one pixel, which is `2^(L+1) · d0`:
//!
//! - level 0 owns distances `[0, 2·d0)`;
//! - level `L ≥ 1` owns `[2^L·d0, 2^(L+1)·d0)`.
//!
//! `d0 = v0 · f_out / bias`, with `v0` the size of a Voxel, `f_out = output_height / (2·tan(vfov/2))`
//! the focal length in output pixels, and `bias` the output pixels one Voxel may cover (1.0 by
//! default; 2.0 is the loose rule that does a quarter of the work for up to one pixel more of
//! error). At 1440p and a 70° vertical field of view that is 51.4 m for 5 cm Voxels.
//!
//! The distance of a brick is that of the nearest point of its box, not of its centre (see
//! [`brick_distance`]). A brick is moved out to the coarser level at 1.05 times the boundary and
//! back in at 0.95 times it ([`HYSTERESIS`]), so a camera that hovers on a boundary does not flip
//! it every frame.
//!
//! All distances are in Voxels of level 0, the unit of the Planet frame, so `v0` is 1 and
//! `d0 = f_out / bias`.
//!
//! **Residency is wider than what is drawn** (ADR 0004, #143). The bricks under a brick that is
//! drawn whole are *wanted* resident before the bands split it: [`Margins`] says how much earlier
//! (a radial factor on the distance, and a camera raised along the Planet's up), and [`Reach`]
//! adds where the camera is going. [`Reach::class`] says which of the three (drawn, margin,
//! prefetch) wants a brick's children, which is the order the loader works in.
//!
//! **The horizon cap** ([`Horizon`], ADR 0004, #146) is the other limit of what is refined: a brick is
//! split, drawn or wanted resident only if some place the camera can be seen from could see a
//! part of it. Nothing beyond the Planet's horizon can be seen, so a brick that is past it is
//! drawn whole at the coarse level that holds it, or not at all, and the store never makes its
//! children. The Guard is not evicted whatever the cap says.
//!
//! **Coarsening** ([`Bands::coarsened`]) is the last resort at the memory cap: the farthest bands
//! use a larger bias, so that fewer bricks are drawn and wanted.

use brick_store::{BrickCoord, VOXELS_PER_METRE};

/// Levels of the pyramid that are drawn: 0 to 13. Level 13's Voxels are 410 m across, which is
/// under a pixel at 800 km, the greatest distance of the Zoom range.
pub const LEVELS: usize = 14;

/// The first level of the Guard: the levels from here to the top of the pyramid (9 to 13, since
/// [`LEVELS`] is 14) are generated for the whole Planet right after the spawn ring (ADR 0004).
/// Level 9 is the one whose Voxels are 25.6 m across, as level 8 was at 10 cm. The client's loader
/// and the prefetch simulator both use it.
pub const GUARD_FIRST: usize = 9;

/// How far past a boundary a brick must go before it changes level, as a fraction of the
/// boundary's distance: out at `1 + HYSTERESIS` times it, back in at `1 - HYSTERESIS` times.
pub const HYSTERESIS: f64 = 0.05;

/// Height in pixels of the image the bands are for: 1440p, the output that the frame budgets are
/// for (the traced image is half of it, and TAAU upscales it).
pub const OUTPUT_HEIGHT: u32 = 1440;

/// The vertical field of view that the bands are for, in radians: the camera's, 70 degrees.
pub const VFOV: f64 = 70.0 * std::f64::consts::PI / 180.0;

/// The band rule for one view: where the boundaries between levels are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    d0: f64,
    coarsening: Option<Coarsening>,
}

/// The farthest bands drawn coarser, to fit the memory budget: the levels from `from` up use a bias
/// `factor` times the rest's, so each of them starts `factor` times nearer. The factor is at most
/// 2, which keeps the bands in order: at 2, level `from - 1` owns no distance at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coarsening {
    pub from: usize,
    pub factor: f64,
}

impl Bands {
    /// The rule for an image `output_height` pixels high with a vertical field of view of `vfov`
    /// radians, where a Voxel may cover `bias` output pixels (1.0 is the strict rule).
    pub fn new(output_height: u32, vfov: f64, bias: f64) -> Self {
        assert!(bias > 0.0 && bias.is_finite(), "bias is {bias}");
        let focal = f64::from(output_height) / (2.0 * (vfov / 2.0).tan());
        Self {
            d0: focal / bias,
            coarsening: None,
        }
    }

    /// The rule for 1440p output and the camera's field of view, with a Voxel allowed `bias`
    /// output pixels. `d0` is then 51.4 m for 5 cm Voxels at a bias of 1.
    pub fn standard(bias: f64) -> Self {
        Self::new(OUTPUT_HEIGHT, VFOV, bias)
    }

    /// The rule with `d0` given directly, in Voxels.
    pub fn with_d0(d0: f64) -> Self {
        assert!(d0 > 0.0 && d0.is_finite(), "d0 is {d0}");
        Self {
            d0,
            coarsening: None,
        }
    }

    /// These bands with the levels from `from` up drawn coarser by `factor` (1 to 2, 1 is none).
    pub fn coarsened(self, from: usize, factor: f64) -> Self {
        assert!(from >= 1, "level 0 has no band to start nearer");
        assert!((1.0..=2.0).contains(&factor), "factor is {factor}");
        Self {
            coarsening: (factor > 1.0).then_some(Coarsening { from, factor }),
            ..self
        }
    }

    /// How the farthest bands are coarsened, if they are.
    pub fn coarsening(&self) -> Option<Coarsening> {
        self.coarsening
    }

    /// The distance at which a Voxel of level 0 covers one output pixel, in Voxels.
    pub fn d0(&self) -> f64 {
        self.d0
    }

    /// The distance at which `level` starts: 0 for level 0, else `2^level · d0` (nearer by the
    /// factor of a [`Coarsening`] for the levels it reaches).
    #[inline]
    pub fn floor(&self, level: usize) -> f64 {
        if level == 0 {
            return 0.0;
        }
        let floor = self.d0 * (1u64 << level) as f64;
        match self.coarsening {
            Some(c) if level >= c.from => floor / c.factor,
            _ => floor,
        }
    }

    /// How many bricks the levels below the Guard hold over flat ground, in units of `d0` squared
    /// (a level owns a ring of ground from `floor(level)` to `floor(level + 1)`, and its bricks
    /// are `8 << level` Voxels across): 4 for level 0 and 3 for each of the others, less for those
    /// that a [`Coarsening`] draws coarser. Only the ratio of two of these means anything: it says
    /// how much more, or less, the bands want than other bands do.
    pub fn weight(&self) -> f64 {
        (0..GUARD_FIRST)
            .map(|level| {
                let (from, to) = (self.floor(level), self.floor(level + 1));
                let across = (8u64 << level) as f64;
                (to * to - from * from) / (across * across)
            })
            .sum::<f64>()
            / (self.d0 * self.d0)
            * 64.0
    }

    /// The level that owns distance `distance`, with no hysteresis, at most `top`.
    #[inline]
    pub fn level_at(&self, distance: f64, top: usize) -> usize {
        (0..top)
            .rev()
            .find(|&level| distance >= self.floor(level + 1))
            .map_or(0, |level| level + 1)
    }

    /// Whether a brick of `level` at `distance` is to be split into the eight of the level
    /// below, because it is nearer than its level is used from. `was_split` is what the brick
    /// was last time, if it was: a brick stays as it was until it is [`HYSTERESIS`] past the
    /// boundary, which is where a camera hovering on one cannot flip it. A brick that is new
    /// is split exactly at the boundary. Level 0 is never split.
    #[inline]
    pub fn split(&self, level: usize, distance: f64, was_split: Option<bool>) -> bool {
        if level == 0 {
            return false;
        }
        let floor = self.floor(level);
        match was_split {
            None => distance < floor,
            Some(false) => distance < floor * (1.0 - HYSTERESIS),
            Some(true) => distance < floor * (1.0 + HYSTERESIS),
        }
    }

    /// How far the camera can move before a brick of `level` at `distance`, which is `split`,
    /// could change that: the distance of a box is 1-Lipschitz in the camera's position, so it
    /// cannot change by more than the camera moves. Infinite for level 0.
    pub fn slack(&self, level: usize, distance: f64, split: bool) -> f64 {
        if level == 0 {
            return f64::INFINITY;
        }
        let floor = self.floor(level);
        let edge = if split {
            floor * (1.0 + HYSTERESIS)
        } else {
            floor * (1.0 - HYSTERESIS)
        };
        (distance - edge).abs()
    }

    /// The radius of the ball about the camera that no brick of `level` is in: all of them are
    /// at least this far. Zero for level 0.
    pub fn clear_radius(&self, level: usize) -> f64 {
        self.floor(level) * (1.0 - HYSTERESIS)
    }
}

/// How much earlier than the bands the bricks under a brick are wanted resident (ADR 0004): the
/// resident set is the drawn cut of the pyramid grown by these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    /// A brick's children are wanted when the brick is nearer than this many times the distance
    /// the bands split it at: 1.25 is a quarter of every band's start.
    pub radial: f64,
    /// The camera also wants what a camera this far higher, along the Planet's radial up, would,
    /// in Voxels.
    pub vertical: f64,
}

impl Margins {
    /// No margin: residency is exactly what is drawn.
    pub const NONE: Margins = Margins {
        radial: 1.0,
        vertical: 0.0,
    };

    /// ADR 0004's: 1.25 radial and 20 m vertical.
    pub fn standard() -> Self {
        Self {
            radial: 1.25,
            vertical: 20.0 * VOXELS_PER_METRE,
        }
    }
}

/// The horizon cap (ADR 0004, #146): what the Planet hides from a camera above it.
///
/// The Planet is a sphere of `radius` Voxels about the origin of the frame: sea level. The ground
/// is nowhere under it (the land is at sea level or above, and the sea is a Material as far as
/// the renderer goes), so a line that crosses the sphere has the ground, or the sea, in front of
/// whatever is past it, and the sphere is an occluder. Two points, `c` and `r` from the centre,
/// see each other only if the distance between them is at most
/// `sqrt(c² - R²) + sqrt(r² - R²)`, the two [`Horizon::tangent`]s, which is the line that grazes
/// the sphere and the longest that does not cross it. So a brick whose box is `d` from the
/// camera, and whose highest point is `r` from the centre, can be seen only if
/// `d <= tangent(c) + tangent(r)`: the brick's own height is in it, so that a peak is seen from
/// beyond the sea's horizon, and a view over a ridge keeps what it looks at.
///
/// A brick that cannot be seen from the camera raised by the vertical margin cannot be seen
/// from the camera, which is below it, so what is drawn is what the raised camera could see:
/// the margin is the part of the horizon that a climb brings in the first 20 m. Residency
/// ([`Class::Margin`] and [`Class::Prefetch`]) reaches the radial margin times further, and
/// from the places that prefetch looks at as well ([`Placed::in_cap_wanted`]).
///
/// A camera that is not above the sphere (under the sea, or dug into the Planet) is not
/// shadowed by it: it has no cap, and everything is as the bands say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Horizon {
    /// Sea level's distance from the centre of the Planet, in Voxels.
    pub radius: f64,
}

impl Horizon {
    /// The cap of a Planet whose sea level is `radius` Voxels from its centre.
    pub fn new(radius: f64) -> Self {
        assert!(radius > 0.0 && radius.is_finite(), "radius is {radius}");
        Self { radius }
    }

    /// The length of the line from a point `r` from the centre that touches the sphere, which is
    /// how far it sees along the ground: 0 for a point on or under it.
    #[inline]
    pub fn tangent(&self, r: f64) -> f64 {
        (r * r - self.radius * self.radius).max(0.0).sqrt()
    }
}

/// The horizon cap seen from one camera, for one of the two reaches that [`Placed`] gives it (what is
/// drawn, and what is wanted resident). A brick whose box is `d` from the camera and whose highest
/// point is `top` from the centre is in it if `hold · (radial · tangent(top) + a) - d` is not
/// negative, with `hold` 1 for a brick that is out and `1 + HYSTERESIS` for one that is in, which is
/// what [`Cap::margin`] says.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cap {
    radius: f64,
    /// The radial margin on the horizon: 1 for what is drawn.
    radial: f64,
    /// `radial · tangent` of the place the cap is seen from, for the farthest of the places, and its
    /// distance from the camera: what the cap reaches that does not depend on the brick.
    a: f64,
    /// The lowest altitude, over the sphere, of the places the cap is seen from, in Voxels (0 if
    /// one is not above it).
    low: f64,
}

impl Cap {
    fn tangent(&self, r: f64) -> f64 {
        (r * r - self.radius * self.radius).max(0.0).sqrt()
    }

    /// How far inside the cap a brick is, in Voxels: negative outside.
    #[inline]
    fn margin(&self, top: f64, distance: f64, was: bool) -> f64 {
        let hold = if was { 1.0 + HYSTERESIS } else { 1.0 };
        hold * (self.radial * self.tangent(top) + self.a) - distance
    }

    /// How far the camera can go (its path, and the places that prefetch looks at, which count as
    /// path too) before a brick that is `margin` inside the cap could be on the other side of it:
    /// a lower bound.
    ///
    /// A unit of path moves the margin by at most `1 + hold · (radial · t' + 1)`, where `t'`, the
    /// growth of a tangent length with altitude (`(R + h) / t(h)`), is the steeper the nearer the
    /// ground: a hundred and more at 20 m over a Planet of 320 km, so a climb of a metre
    /// brings the horizon in by a hundred. The tangent length is concave in the altitude, so
    /// its slope at the lowest place's altitude is the bound while the path stays above half of it
    /// (`near`); and it is subadditive, so a climb of `x` adds no more than `t(x)` whatever the
    /// altitude it starts at (`far`), which is what bounds a brick that is far from the edge.
    fn slack(&self, margin: f64) -> f64 {
        let hold = 1.0 + HYSTERESIS;
        let margin = margin.abs();
        // The altitude of a place moves with the camera's, and a little more (the up it is raised
        // along turns): 0.1% is more than 1 + 400 / 6 400 000.
        const EXTRA: f64 = 1.001;
        let half = self.radius + self.low / 2.0;
        let slope_of_tangent = half / self.tangent(half).max(f64::MIN_POSITIVE);
        let slope = 1.0 + hold * (self.radial * slope_of_tangent + 1.0);
        let near = (margin / (slope * EXTRA)).min(self.low / 2.0 / EXTRA);
        // Half of the margin for the tangent, half for the rest of the path: the tangent grows by
        // `g` in a climb of `x(g)`.
        let g = margin / (2.0 * hold * self.radial);
        let climb = g * g / ((self.radius * self.radius + g * g).sqrt() + self.radius);
        let far = (climb / EXTRA).min(margin / (2.0 * (1.0 + hold) * EXTRA));
        near.max(far).max(0.0)
    }
}

/// Places that prefetch looks at, along the camera's velocity: [`Reach::ahead`] holds this many.
pub const AHEAD: usize = 2;

/// Which wants the children of a brick resident, most urgent last (the loader works in this order
/// before distance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// Only where the camera is going, a second or two on: never relied on.
    Prefetch,
    /// The margins about the camera.
    Margin,
    /// The bands split the brick: it is drawn whole only until they arrive.
    Drawn,
}

/// What residency looks at besides what is drawn: the [`Margins`], and where the camera will be
/// ([`AHEAD`] positions on its velocity, which the client works out from the frames it has seen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reach {
    pub margins: Margins,
    pub ahead: [Option<[f64; 3]>; AHEAD],
    /// The horizon cap, if there is one (see [`Horizon`]).
    pub horizon: Option<Horizon>,
}

impl Reach {
    /// What is drawn and nothing else.
    pub const NONE: Reach = Reach {
        margins: Margins::NONE,
        ahead: [None; AHEAD],
        horizon: None,
    };

    /// Whether this asks for nothing beyond the bands: no margins and no lead. (The horizon cap
    /// takes away from what the bands ask for, and is not asked about here.)
    pub fn is_none(&self) -> bool {
        self.margins == Margins::NONE && self.ahead == [None; AHEAD]
    }

    /// Whether the margins ask for anything beyond the bands.
    pub fn has_margins(&self) -> bool {
        self.margins != Margins::NONE
    }

    /// `camera` raised by the vertical margin, along the Planet's radial up (the Planet is a
    /// sphere about the origin of the frame).
    pub fn raised(&self, camera: [f64; 3]) -> [f64; 3] {
        let length = camera.iter().map(|c| c * c).sum::<f64>().sqrt();
        let up = if length > 1.0 {
            camera.map(|c| c / length)
        } else {
            [0.0, 1.0, 0.0]
        };
        std::array::from_fn(|a| camera[a] + up[a] * self.margins.vertical)
    }

    /// How high the raised camera is above the sphere of the horizon cap, in Voxels, if there is a
    /// horizon: the cap is on while this is above nothing.
    pub fn height(&self, camera: [f64; 3]) -> Option<f64> {
        let horizon = self.horizon?;
        let raised = self.raised(camera);
        Some(raised.iter().map(|c| c * c).sum::<f64>().sqrt() - horizon.radius)
    }

    /// This reach seen from `camera`, with the raised camera worked out once for the many bricks
    /// that are asked about.
    pub fn from(&self, camera: [f64; 3]) -> Placed {
        let raised = self.raised(camera);
        let (drawn, wanted) = self.caps(camera, raised);
        Placed {
            reach: *self,
            camera,
            raised,
            drawn,
            wanted,
        }
    }

    /// The caps of what is drawn and of what is wanted resident, from `camera` and the places it
    /// is raised to: none if there is no horizon, or the raised camera is not above the sphere.
    fn caps(&self, camera: [f64; 3], raised: [f64; 3]) -> (Option<Cap>, Option<Cap>) {
        let Some(horizon) = self.horizon else {
            return (None, None);
        };
        let norm = |p: [f64; 3]| p.iter().map(|c| c * c).sum::<f64>().sqrt();
        let height = norm(raised) - horizon.radius;
        if height <= 0.0 {
            return (None, None);
        }
        let from_camera = |p: [f64; 3]| norm(std::array::from_fn(|a| p[a] - camera[a]));
        let cap = |radial: f64, a: f64, low: f64| Cap {
            radius: horizon.radius,
            radial,
            a,
            low,
        };
        let drawn = cap(
            1.0,
            horizon.tangent(norm(raised)) + from_camera(raised),
            height,
        );
        let radial = self.margins.radial;
        let place = |p: [f64; 3]| radial * horizon.tangent(norm(p)) + from_camera(p);
        let (mut a, mut low) = (place(raised), height);
        for &at in self.ahead.iter().flatten() {
            let at = self.raised(at);
            a = a.max(place(at));
            low = low.min(norm(at) - horizon.radius);
        }
        (Some(drawn), Some(cap(radial, a, low.max(0.0))))
    }

    /// [`Placed::distance`], for one brick.
    pub fn distance(&self, camera: [f64; 3], level: usize, coord: BrickCoord) -> f64 {
        self.from(camera).distance(level, coord)
    }

    /// [`Placed::class`], for one brick.
    pub fn class(
        &self,
        bands: Bands,
        camera: [f64; 3],
        level: usize,
        coord: BrickCoord,
    ) -> Option<Class> {
        self.from(camera).class(bands, level, coord)
    }
}

/// A [`Reach`] seen from one camera: ask it about many bricks without working out where the
/// camera raised is each time.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    reach: Reach,
    camera: [f64; 3],
    raised: [f64; 3],
    /// The horizon cap of what is drawn, and of what is wanted resident (see [`Horizon`]).
    drawn: Option<Cap>,
    wanted: Option<Cap>,
}

impl Placed {
    /// The camera.
    pub fn camera(&self) -> [f64; 3] {
        self.camera
    }

    /// Whether the horizon cap is on: there is one, and the camera raised is above its sphere.
    pub fn has_cap(&self) -> bool {
        self.drawn.is_some()
    }

    /// Whether the horizon cap leaves the brick at `coord` of `level` in what is drawn: the camera
    /// raised by the vertical margin could see a part of it. `was` is whether it was in already:
    /// it is held in until the camera is [`HYSTERESIS`] past. Always true with no cap.
    #[inline]
    pub fn in_cap(&self, level: usize, coord: BrickCoord, was: bool) -> bool {
        self.in_cap_at(level, coord, brick_distance(self.camera, level, coord), was)
    }

    /// [`Placed::in_cap`] for a brick that is `distance` from the camera.
    #[inline]
    pub fn in_cap_at(&self, level: usize, coord: BrickCoord, distance: f64, was: bool) -> bool {
        self.drawn
            .is_none_or(|cap| cap.margin(brick_top(level, coord), distance, was) >= 0.0)
    }

    /// How far the camera can go before [`Placed::in_cap_at`] could say otherwise, in Voxels (the
    /// path of the camera, and of the places that prefetch looks at): infinite with no cap.
    pub fn cap_slack(&self, level: usize, coord: BrickCoord, distance: f64, was: bool) -> f64 {
        self.drawn.map_or(f64::INFINITY, |cap| {
            cap.slack(cap.margin(brick_top(level, coord), distance, was))
        })
    }

    /// [`Placed::in_cap`] for what is wanted resident: the horizon of the camera raised and of the
    /// places it is going to, times the radial margin.
    #[inline]
    pub fn in_cap_wanted(&self, level: usize, coord: BrickCoord, was: bool) -> bool {
        self.wanted.is_none_or(|cap| {
            cap.margin(
                brick_top(level, coord),
                brick_distance(self.camera, level, coord),
                was,
            ) >= 0.0
        })
    }

    /// [`Placed::cap_slack`] for what is wanted resident.
    pub fn cap_slack_wanted(
        &self,
        level: usize,
        coord: BrickCoord,
        distance: f64,
        was: bool,
    ) -> f64 {
        self.wanted.map_or(f64::INFINITY, |cap| {
            cap.slack(cap.margin(brick_top(level, coord), distance, was))
        })
    }

    /// Whether the reach asks for nothing beyond the bands.
    pub fn is_none(&self) -> bool {
        self.reach.is_none()
    }

    /// The distance the bands are applied to for residency: the nearer of the camera and the camera
    /// raised, over the radial margin, or that of a place the camera is going, whichever is
    /// least. A brick whose children are wanted is one the bands split at this distance. It moves
    /// no faster than the places it is measured from do.
    pub fn distance(&self, level: usize, coord: BrickCoord) -> f64 {
        let mut near = brick_distance(self.camera, level, coord);
        if self.reach.margins.vertical > 0.0 {
            near = near.min(brick_distance(self.raised, level, coord));
        }
        self.reach
            .ahead
            .iter()
            .flatten()
            .fold(near / self.reach.margins.radial, |d, &at| {
                d.min(brick_distance(at, level, coord))
            })
    }

    /// Who wants the children of the brick at `coord` of `level`, with the bands applied as
    /// [`Bands::split`] does to a brick that is split already (it stays wanted until the camera is
    /// [`HYSTERESIS`] past the boundary). `None` if nothing does. The bands do not split a brick
    /// that the horizon cap leaves out (nothing could see what is under it, so it is drawn whole,
    /// or not at all), and the margin and the lead do not want what is outside their own, wider cap.
    pub fn class(&self, bands: Bands, level: usize, coord: BrickCoord) -> Option<Class> {
        let reach = &self.reach;
        let drawn = brick_distance(self.camera, level, coord);
        if bands.split(level, drawn, Some(true)) && self.in_cap_at(level, coord, drawn, true) {
            return Some(Class::Drawn);
        }
        if !self.in_cap_wanted(level, coord, true) {
            return None;
        }
        if reach.has_margins() {
            let mut near = drawn;
            if reach.margins.vertical > 0.0 {
                near = near.min(brick_distance(self.raised, level, coord));
            }
            if bands.split(level, near / reach.margins.radial, Some(true)) {
                return Some(Class::Margin);
            }
        }
        reach
            .ahead
            .iter()
            .flatten()
            .any(|&at| bands.split(level, brick_distance(at, level, coord), Some(true)))
            .then_some(Class::Prefetch)
    }
}

/// The distance from the centre of the Planet (the origin of the frame) to the farthest point of
/// the box of the brick at `coord` of mip `level`: how high the brick reaches, for the horizon cap.
#[inline]
pub fn brick_top(level: usize, coord: BrickCoord) -> f64 {
    let size = (8u64 << level) as f64;
    let mut sum = 0.0;
    for &b in &coord.0 {
        let low = f64::from(b) * size;
        let far = low.abs().max((low + size).abs());
        sum += far * far;
    }
    sum.sqrt()
}

/// The distance from `camera`, a position in Voxels, to the nearest point of the box of the
/// brick at `coord` of mip `level`: 0 if the camera is inside it.
#[inline]
pub fn brick_distance(camera: [f64; 3], level: usize, coord: BrickCoord) -> f64 {
    let size = (8u64 << level) as f64;
    let mut sum = 0.0;
    for (&at, &b) in camera.iter().zip(&coord.0) {
        let low = f64::from(b) * size;
        let outside = (low - at).max(at - (low + size)).max(0.0);
        sum += outside * outside;
    }
    sum.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::VOXELS_PER_METRE;

    const VFOV: f64 = 70.0 * std::f64::consts::PI / 180.0;

    #[test]
    fn d0_is_a_voxel_times_the_focal_length_over_the_bias() {
        // 1440p, 70 degrees: f is 1028.2 pixels, so a 5 cm Voxel is one pixel at 51.4 m.
        let bands = Bands::new(1440, VFOV, 1.0);
        assert!((bands.d0() - 1028.2).abs() < 0.1, "{}", bands.d0());
        // In metres: a Voxel is 1 / VOXELS_PER_METRE of one.
        assert!((bands.d0() / VOXELS_PER_METRE - 51.4).abs() < 0.05);
        // The bias scales every distance.
        assert!((Bands::new(1440, VFOV, 2.0).d0() * 2.0 - bands.d0()).abs() < 1e-9);
        // So does the image height, and a wider view shortens them.
        assert!((Bands::new(720, VFOV, 1.0).d0() * 2.0 - bands.d0()).abs() < 1e-9);
        assert!(Bands::new(1440, 2.0 * VFOV / 1.5, 1.0).d0() < bands.d0());
    }

    #[test]
    fn the_pyramid_reaches_the_farthest_zoom_distance_and_no_further() {
        // A level owns everything beyond its floor, so the top level must start before the Zoom
        // range's 800 km, and one more would only start beyond it: the levels follow from the
        // size of a Voxel.
        let bands = Bands::new(1440, VFOV, 1.0);
        let farthest = 800_000.0 * VOXELS_PER_METRE;
        assert!(bands.floor(LEVELS - 1) <= farthest);
        assert!(bands.floor(LEVELS) > farthest);
    }

    #[test]
    fn level_0_owns_two_d0_and_every_level_above_owns_a_doubling() {
        let bands = Bands::with_d0(100.0);
        assert_eq!(bands.floor(0), 0.0);
        assert_eq!(bands.floor(1), 200.0);
        assert_eq!(bands.floor(2), 400.0);
        assert_eq!(bands.floor(12), 100.0 * 4096.0);
        for (distance, level) in [
            (0.0, 0),
            (199.9, 0),
            (200.0, 1),
            (399.9, 1),
            (400.0, 2),
            (799.9, 2),
            (800.0, 3),
            (1e9, 12),
        ] {
            assert_eq!(bands.level_at(distance, 12), level, "{distance}");
        }
        // A pyramid with fewer levels stops at its top.
        assert_eq!(bands.level_at(1e9, 4), 4);
        assert_eq!(bands.level_at(1e9, 0), 0);
    }

    #[test]
    fn a_new_brick_splits_exactly_at_its_boundary_and_level_0_never_does() {
        let bands = Bands::with_d0(100.0);
        assert!(bands.split(1, 199.99, None));
        assert!(!bands.split(1, 200.0, None));
        assert!(bands.split(5, 0.0, None));
        assert!(!bands.split(0, 0.0, None));
        assert!(!bands.split(0, 0.0, Some(true)));
    }

    #[test]
    fn a_brick_changes_level_only_five_percent_past_the_boundary() {
        let bands = Bands::with_d0(100.0);
        // Level 1 starts at 200. Out of it (merged into the coarser brick) at 210, back at 190.
        assert!(bands.split(1, 209.9, Some(true)));
        assert!(!bands.split(1, 210.0, Some(true)));
        assert!(!bands.split(1, 190.0, Some(false)));
        assert!(bands.split(1, 189.9, Some(false)));
        // Between 190 and 210 a brick stays as it was.
        for distance in [190.0, 199.0, 200.0, 205.0, 209.9] {
            assert!(bands.split(1, distance, Some(true)), "{distance}");
        }
        for distance in [190.0, 195.0, 200.0, 209.9, 400.0] {
            assert!(!bands.split(1, distance, Some(false)), "{distance}");
        }
    }

    #[test]
    fn a_camera_hovering_on_a_boundary_never_flips_a_brick() {
        let bands = Bands::with_d0(100.0);
        for start in [true, false] {
            let mut state = start;
            let mut flips = 0;
            // Bobbing 4% either side of the boundary at 400 (level 2), 100 times.
            for i in 0..100 {
                let distance = 400.0 * if i % 2 == 0 { 0.96 } else { 1.04 };
                let next = bands.split(2, distance, Some(state));
                flips += usize::from(next != state);
                state = next;
            }
            assert_eq!(
                flips,
                0,
                "starting {}",
                if start { "split" } else { "whole" }
            );
        }
        // Past the 5%, it flips once each way.
        let mut state = false;
        let mut flips = 0;
        for distance in [420.0, 380.0, 360.0, 380.0, 410.0, 430.0] {
            let next = bands.split(2, distance, Some(state));
            flips += usize::from(next != state);
            state = next;
        }
        assert_eq!(flips, 2);
    }

    #[test]
    fn the_slack_is_how_far_the_camera_must_go_to_reach_the_edge_of_the_hysteresis() {
        let bands = Bands::with_d0(100.0);
        // Whole at 460 (level 2 starts at 400, and it is whole until 380 is crossed): 80 to go.
        assert!((bands.slack(2, 460.0, false) - 80.0).abs() < 1e-9);
        // Split, at 300: it merges at 420, 120 away.
        assert!((bands.slack(2, 300.0, true) - 120.0).abs() < 1e-9);
        assert!(bands.slack(0, 5.0, false).is_infinite());
    }

    #[test]
    fn a_ball_about_the_camera_has_no_brick_of_the_level_in_it() {
        let bands = Bands::with_d0(100.0);
        assert_eq!(bands.clear_radius(0), 0.0);
        assert!((bands.clear_radius(3) - 760.0).abs() < 1e-9);
    }

    #[test]
    fn a_brick_is_as_far_as_the_nearest_point_of_its_box() {
        // Level 1: 16 Voxels across. Brick (1, 0, 0) is x 16..32, y 0..16, z 0..16.
        let coord = BrickCoord([1, 0, 0]);
        assert_eq!(brick_distance([20.0, 5.0, 5.0], 1, coord), 0.0);
        assert_eq!(brick_distance([40.0, 5.0, 5.0], 1, coord), 8.0);
        assert_eq!(brick_distance([0.0, 5.0, 5.0], 1, coord), 16.0);
        // Off a corner, along the diagonal.
        let corner = brick_distance([35.0, 19.0, -4.0], 1, coord);
        assert!((corner - (3.0f64 * 3.0 + 3.0 * 3.0 + 4.0 * 4.0).sqrt()).abs() < 1e-12);
        // Negative bricks, and a level that is not 1.
        assert_eq!(
            brick_distance([0.0, 0.0, 0.0], 0, BrickCoord([-1, 0, 0])),
            0.0
        );
        assert_eq!(
            brick_distance([4.0, 0.0, 0.0], 2, BrickCoord([-1, 0, 0])),
            4.0
        );
    }

    #[test]
    fn the_distance_moves_no_faster_than_the_camera() {
        // What `slack` relies on.
        let coord = BrickCoord([3, -2, 5]);
        let mut state = 7u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 40) as f64 / (1u64 << 24) as f64 * 400.0 - 200.0
        };
        for level in 0..4 {
            for _ in 0..500 {
                let a = [next(), next(), next()];
                let b = [next(), next(), next()];
                let moved = (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt();
                let change =
                    (brick_distance(a, level, coord) - brick_distance(b, level, coord)).abs();
                assert!(change <= moved + 1e-9);
            }
        }
    }

    #[test]
    fn coarsening_brings_the_farthest_bands_in_and_keeps_them_in_order() {
        let bands = Bands::with_d0(100.0);
        let coarse = bands.coarsened(3, 1.5);
        for level in 0..3 {
            assert_eq!(coarse.floor(level), bands.floor(level));
        }
        assert!((coarse.floor(3) - 800.0 / 1.5).abs() < 1e-9);
        assert!((coarse.floor(5) - 3200.0 / 1.5).abs() < 1e-9);
        assert_eq!(coarse.level_at(500.0, 8), 2);
        assert_eq!(coarse.level_at(540.0, 8), 3);
        // At 2 the band under the first level coarsened is gone: level 2 owns nothing.
        let most = bands.coarsened(3, 2.0);
        assert_eq!(most.floor(3), most.floor(2));
        assert_eq!(most.level_at(450.0, 8), 3);
        // No factor, no coarsening, and the bands are the plain ones.
        assert_eq!(bands.coarsened(3, 1.0), bands);
        for level in 1..10 {
            assert!(coarse.floor(level) >= coarse.floor(level - 1));
        }
    }

    #[test]
    fn a_brick_is_wanted_by_the_margin_before_the_bands_split_it_and_by_the_lead_later() {
        let bands = Bands::with_d0(100.0);
        let reach = Reach {
            margins: Margins {
                radial: 1.25,
                vertical: 0.0,
            },
            ahead: [Some([1000.0, 0.0, 0.0]), None],
            horizon: None,
        };
        let camera = [0.0, 0.0, 0.0];
        let at = |x: i32| BrickCoord([x, 0, 0]);
        // Level 1 starts at 200, bricks are 16 Voxels across: the one at the camera is drawn
        // split, one at 224 (x = 14) is whole but under 250 (1.25 times 200): the margin.
        assert_eq!(reach.class(bands, camera, 1, at(0)), Some(Class::Drawn));
        assert_eq!(reach.class(bands, camera, 1, at(14)), Some(Class::Margin));
        // 304 away: beyond the margin, and 700 from the place the camera is going.
        assert_eq!(reach.class(bands, camera, 1, at(19)), None);
        // Level 4 starts at 1600 and its bricks are 128 across: x = 14 is 1792 away, in the margin.
        assert_eq!(
            reach.class(bands, camera, 4, BrickCoord([14, 0, 0])),
            Some(Class::Margin)
        );
        // Near the place the camera is going, and nowhere else, only the lead wants it.
        assert_eq!(reach.class(bands, camera, 1, at(62)), Some(Class::Prefetch));
        // With no reach beyond the bands only the bands want it.
        assert_eq!(Reach::NONE.class(bands, camera, 1, at(14)), None);
        assert_eq!(
            Reach::NONE.class(bands, camera, 1, at(0)),
            Some(Class::Drawn)
        );
        assert!(Class::Drawn > Class::Margin && Class::Margin > Class::Prefetch);
    }

    #[test]
    fn the_weight_of_bands_falls_with_coarsening_and_not_with_d0_squared() {
        let plain = Bands::with_d0(1000.0);
        assert!((plain.weight() - 28.0).abs() < 1e-9, "{}", plain.weight());
        assert!((Bands::with_d0(500.0).weight() - plain.weight()).abs() < 1e-9);
        let mut last = plain.weight();
        for (from, factor) in [(8, 1.5), (7, 1.5), (6, 1.5), (5, 1.5), (4, 2.0), (2, 2.0)] {
            let weight = plain.coarsened(from, factor).weight();
            assert!(weight < last, "{from} {factor}: {weight} {last}");
            last = weight;
        }
    }

    #[test]
    fn the_vertical_margin_is_a_camera_raised_along_the_radial_up() {
        let reach = Reach {
            margins: Margins {
                radial: 1.0,
                vertical: 400.0,
            },
            ahead: [None; AHEAD],
            horizon: None,
        };
        // On the +x axis the radial up is +x.
        let up = reach.raised([5000.0, 0.0, 0.0]);
        assert!((up[0] - 5400.0).abs() < 1e-9 && up[1] == 0.0 && up[2] == 0.0);
        // A brick above the camera is nearer to the raised camera than to the camera.
        let above = BrickCoord([700, 0, 0]);
        let plain = brick_distance([5000.0, 0.0, 0.0], 0, above);
        let raised = reach.distance([5000.0, 0.0, 0.0], 0, above);
        assert!((plain - raised - 400.0).abs() < 1e-9, "{plain} {raised}");
        // At the origin there is no up to take: a fixed one is used instead of a NaN.
        assert!(reach.raised([0.0; 3]).iter().all(|c| c.is_finite()));
    }

    /// The Planet of the client: 320 km of 5 cm Voxels.
    const SEA: f64 = 320_000.0 * VOXELS_PER_METRE;

    fn capped(vertical: f64) -> Reach {
        Reach {
            margins: Margins {
                radial: 1.25,
                vertical,
            },
            ahead: [None; AHEAD],
            horizon: Some(Horizon::new(SEA)),
        }
    }

    #[test]
    fn the_horizon_is_a_kilometre_and_a_tenth_from_the_ground_of_320_km() {
        let horizon = Horizon::new(SEA);
        // 2 m up: sqrt(2 R h) = 1131 m. 20 m up: 3578 m.
        let metres = |up: f64| horizon.tangent(SEA + up * VOXELS_PER_METRE) / VOXELS_PER_METRE;
        assert!((metres(2.0) - 1131.4).abs() < 0.5, "{}", metres(2.0));
        assert!((metres(20.0) - 3577.7).abs() < 1.0, "{}", metres(20.0));
        assert_eq!(horizon.tangent(SEA - 10.0), 0.0);
    }

    #[test]
    fn a_brick_is_in_the_cap_by_how_far_it_and_the_camera_see_along_the_ground() {
        // A camera 2 m up (the vertical margin raises it 20 m: the horizon is 3.6 km), on the +y
        // axis. A brick on the ground a distance `along` away.
        let reach = capped(20.0 * VOXELS_PER_METRE);
        let camera = [0.0, SEA + 2.0 * VOXELS_PER_METRE, 0.0];
        let at = reach.from(camera);
        assert!(at.has_cap());
        let on_the_ground = |metres: f64| {
            let angle = metres * VOXELS_PER_METRE / SEA;
            BrickCoord([
                (SEA * angle.sin() / 8.0) as i32,
                (SEA * angle.cos() / 8.0) as i32,
                0,
            ])
        };
        // The raised camera, 22 m up, sees 3.75 km along the ground (and 20 m of the distance is
        // the way up), and the brick's own height, 0.25 m, adds 0.4 km: 4.17 km. 3 km on: in. 4.6
        // km on: out.
        assert!(at.in_cap(0, on_the_ground(3000.0), false));
        assert!(!at.in_cap(0, on_the_ground(4600.0), false));
        // A brick that was in is held in until the camera is a twentieth further.
        assert!(!at.in_cap(0, on_the_ground(4250.0), false));
        assert!(at.in_cap(0, on_the_ground(4250.0), true));
        // A ridge: the same place 400 m up is seen from 22 km further (sqrt(2 R 400 m) = 16 km).
        let on_a_ridge = {
            let angle = 15_000.0 * VOXELS_PER_METRE / SEA;
            let r = SEA + 400.0 * VOXELS_PER_METRE;
            BrickCoord([
                (r * angle.sin() / 8.0) as i32,
                (r * angle.cos() / 8.0) as i32,
                0,
            ])
        };
        assert!(at.in_cap(0, on_a_ridge, false));
        // The camera under the sea has no cap, and neither has a reach with no horizon.
        assert!(!reach.from([0.0, SEA - 3000.0, 0.0]).has_cap());
        assert!(
            Reach::NONE
                .from(camera)
                .in_cap(0, on_the_ground(500_000.0), false)
        );
    }

    #[test]
    fn what_the_cap_wants_resident_reaches_the_radial_margin_further_and_covers_what_is_drawn() {
        let reach = capped(20.0 * VOXELS_PER_METRE);
        let camera = [0.0, SEA + 2.0 * VOXELS_PER_METRE, 0.0];
        let at = reach.from(camera);
        let on_the_ground = |metres: f64| {
            let angle = metres * VOXELS_PER_METRE / SEA;
            BrickCoord([
                (SEA * angle.sin() / 8.0) as i32,
                (SEA * angle.cos() / 8.0) as i32,
                0,
            ])
        };
        // Drawn out to 4.17 km, wanted out to 1.25 times the raised camera's tangent and the
        // brick's (5.1 km), and never less than what is drawn.
        for metres in (0..8000).step_by(50) {
            let brick = on_the_ground(f64::from(metres));
            assert!(
                at.in_cap_wanted(0, brick, false) || !at.in_cap(0, brick, false),
                "{metres}"
            );
        }
        assert!(!at.in_cap(0, on_the_ground(4700.0), false));
        assert!(at.in_cap_wanted(0, on_the_ground(4700.0), false));
        assert!(!at.in_cap_wanted(0, on_the_ground(6000.0), false));
    }

    #[test]
    fn the_camera_cannot_change_a_bricks_side_of_the_cap_in_less_than_its_slack() {
        // Random bricks about a random camera over the Planet, at every height from 2 m to 100 km:
        // go the slack in random directions, and in the two that matter, straight up and straight
        // down, and the brick is where it was, drawn and wanted.
        let reach = capped(20.0 * VOXELS_PER_METRE);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut checked = 0;
        for _ in 0..4000 {
            let up = 40.0 * 2f64.powf(next() * 15.0);
            let tilt = next() * 0.2;
            let direction = |a: f64, b: f64| {
                let v = [a.sin() * b.cos(), a.cos(), a.sin() * b.sin()];
                let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                v.map(|c| c / n)
            };
            let up_dir = direction(tilt, next() * std::f64::consts::TAU);
            let camera = up_dir.map(|c| c * (SEA + up));
            let at = reach.from(camera);
            // A brick about as far as the cap reaches.
            let level = (next() * 6.0) as usize;
            let reachable = at.cap_slack(level, BrickCoord([0, 0, 0]), 0.0, false);
            let _ = reachable;
            let angle =
                (next() * 2.0 - 1.0) * (reach.height(camera).unwrap_or(0.0) / SEA).sqrt() * 3.0
                    + tilt;
            let spot = direction(angle.abs().max(1e-9), next() * std::f64::consts::TAU);
            let r = SEA + (next() * 2.0 - 0.2) * 5000.0 * VOXELS_PER_METRE;
            let size = (8u64 << level) as f64;
            let coord = BrickCoord(spot.map(|c| (c * r / size).floor() as i32));
            let d = brick_distance(camera, level, coord);
            for was in [false, true] {
                let now = (
                    at.in_cap(level, coord, was),
                    at.in_cap_wanted(level, coord, was),
                );
                let slack = at
                    .cap_slack(level, coord, d, was)
                    .min(at.cap_slack_wanted(level, coord, d, was));
                if !slack.is_finite() || slack > 1.0e7 {
                    continue;
                }
                checked += 1;
                for step in [
                    up_dir,
                    up_dir.map(|c| -c),
                    direction(
                        next() * std::f64::consts::PI,
                        next() * std::f64::consts::TAU,
                    ),
                ] {
                    let moved: [f64; 3] =
                        std::array::from_fn(|a| camera[a] + step[a] * slack * 0.999);
                    let after = reach.from(moved);
                    // Down under the sea the cap is off, which the selection looks at on its own.
                    if !after.has_cap() {
                        continue;
                    }
                    // The brick's distance changed, and so must be asked of the new camera.
                    let state = (
                        after.in_cap(level, coord, was),
                        after.in_cap_wanted(level, coord, was),
                    );
                    assert_eq!(
                        state,
                        now,
                        "level {level} {coord:?} was {was}: slack {slack} at {up} up; margin {:?} then {:?}, step {step:?}, d {d} then {}",
                        at.drawn.map(|c| c.margin(brick_top(level, coord), d, was)),
                        after.drawn.map(|c| c.margin(
                            brick_top(level, coord),
                            brick_distance(moved, level, coord),
                            was
                        )),
                        brick_distance(moved, level, coord)
                    );
                }
            }
        }
        assert!(checked > 2000, "{checked}");
    }

    #[test]
    fn nothing_that_a_camera_can_see_is_outside_the_cap_whatever_the_ridge_or_the_height() {
        // Random cameras over the Planet (2 m to 20 km up, and under the 20 m of the margin: the
        // cap is for the raised camera) and random points of the ground and above it (peaks of
        // 0 to 600 m), at every bearing and range: if the segment between them does not cross the
        // sphere of the sea, the brick at that point, at every level, is in the cap.
        let reach = capped(20.0 * VOXELS_PER_METRE);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let unit = |a: f64, b: f64| [a.sin() * b.cos(), a.cos(), a.sin() * b.sin()];
        let mut seen = 0;
        for _ in 0..20_000 {
            let up = 2.0 * VOXELS_PER_METRE * 10f64.powf(next() * 4.0);
            let camera = unit(next() * 0.1, next() * std::f64::consts::TAU).map(|c| c * (SEA + up));
            let at = reach.from(camera);
            let peak = next() * 600.0 * VOXELS_PER_METRE;
            // Range of the line of sight at most, about, so most of them see the point.
            let range = (2.0 * SEA * (up + peak)).sqrt() * 1.2 * next();
            let direction = unit((range / SEA).max(1e-9), next() * std::f64::consts::TAU);
            let point = direction.map(|c| c * (SEA + peak));
            // The distance from the centre of the nearest point of the segment from the camera.
            let along: [f64; 3] = std::array::from_fn(|a| point[a] - camera[a]);
            let length2 = along.iter().map(|v| v * v).sum::<f64>();
            let t = (-camera.iter().zip(&along).map(|(c, a)| c * a).sum::<f64>() / length2)
                .clamp(0.0, 1.0);
            let nearest: f64 = (0..3)
                .map(|a| (camera[a] + along[a] * t).powi(2))
                .sum::<f64>()
                .sqrt();
            if nearest < SEA {
                continue;
            }
            seen += 1;
            for level in 0..6 {
                let size = (8u64 << level) as f64;
                let coord = BrickCoord(point.map(|c| (c / size).floor() as i32));
                assert!(
                    at.in_cap(level, coord, false),
                    "level {level}, {up} up, point {peak} high at {range}"
                );
            }
        }
        assert!(seen > 5_000, "{seen}");
    }
}
