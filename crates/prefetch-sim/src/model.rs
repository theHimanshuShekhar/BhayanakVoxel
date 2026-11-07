//! The world the simulator replays paths through: the Planet as a sphere of surface bricks, the
//! LOD band rule, the shell's margins and the Guard.
//!
//! Everything is in Voxels of level 0 (the unit of the Planet frame and of [`render::lod`]), so
//! a metre is [`VOXELS_PER_METRE`] of them. The Planet is a sphere about the origin.

use render::lod::{Bands, LEVELS};

/// Voxels of level 0 in a metre: Voxels are 10 cm.
pub const VOXELS_PER_METRE: f64 = 10.0;

/// Bricks a full fill draws, in units of d0 squared (a level's bricks are the surface within its
/// band, so they go with d0²): measured by the client on the full-size Planet, 3.21 million at a
/// bias of 1 (`BRICKS_PER_D0_SQUARED` in `bins/client/src/main.rs`).
pub const DRAWN_PER_D0_SQUARED: f64 = 3.04;

/// The model's [`Model::crust`].
pub const CRUST: f64 = 0.4;

/// The eye's height above the ground when walking or flying low: 2 m.
pub const EYE: f64 = 2.0 * VOXELS_PER_METRE;

/// What is resident, what is required, and by which rules.
#[derive(Clone, Debug)]
pub struct Model {
    /// Radius of the ground: sea level, [`planet_gen::world_map::WORLD_RADIUS`] for the real Planet.
    pub planet_radius: f64,
    /// How far the ground's relief reaches either side of the sphere. A brick is a surface brick
    /// if it meets the shell of this half-thickness: more is rougher terrain (the research's
    /// k = 2: slopes and overhangs have about twice the flat surface). It also raises the
    /// horizon, since a peak this high shows from beyond the sphere's.
    pub relief: f64,
    /// How far beyond the sphere a brick can be and still be surface, as a fraction of the
    /// brick's own edge: the real Planet draws more than the bricks that touch a smooth sphere
    /// (hills, strata boundaries, caves, and coarse bricks that read as air with ground under
    /// them), and more at every level alike. Calibrated so that a full fill draws what the client
    /// measured, [`DRAWN_PER_D0_SQUARED`] d0² bricks (the test `a_full_fill_draws_what_the_client_measured`);
    /// it does not move the horizon.
    pub crust: f64,
    /// The band rule (#84): which level a brick is drawn at, from its distance.
    pub bands: Bands,
    /// Levels of the pyramid that are drawn, as [`render::lod::LEVELS`].
    pub levels: usize,
    /// The first level of the Guard, which is always resident, to the top level; `None` for no
    /// Guard at all.
    pub guard_first: Option<usize>,
    /// The shell's radial margin: it reaches this many times the horizon.
    pub radial_margin: f64,
    /// The shell's vertical margin, in Voxels: it is also built for a camera this much higher.
    pub vertical_margin: f64,
    /// Prefetch: the lead time, in seconds, is `2 · horizon / speed` clamped to this range. `None`
    /// turns velocity prefetch off.
    pub lead: Option<(f64, f64)>,
    /// Half-angle tangents of the view, horizontal then vertical: 70 degrees vertical at 16:9.
    pub tan_half_fov: [f64; 2],
}

impl Model {
    /// The real Planet, with the ADR 0004 margins, the Guard at levels 8 to the top, the
    /// renderer's strict band rule at 1440p, and a 1 to 2 s prefetch lead.
    pub fn real() -> Self {
        let tan_v = (render::lod::VFOV / 2.0).tan();
        Self {
            planet_radius: f64::from(planet_gen::world_map::WORLD_RADIUS),
            relief: 0.0,
            crust: CRUST,
            bands: Bands::standard(1.0),
            levels: LEVELS,
            guard_first: Some(8),
            radial_margin: 1.25,
            vertical_margin: 20.0 * VOXELS_PER_METRE,
            lead: Some((1.0, 2.0)),
            tan_half_fov: [tan_v * 16.0 / 9.0, tan_v],
        }
    }

    /// The top level of the pyramid.
    pub fn top(&self) -> usize {
        self.levels - 1
    }

    /// Whether `level` is part of the Guard: always resident.
    pub fn in_guard(&self, level: usize) -> bool {
        self.guard_first.is_some_and(|first| level >= first)
    }

    /// A camera's height above sea level, in Voxels, from its position.
    pub fn altitude(&self, pos: [f64; 3]) -> f64 {
        norm(pos) - self.planet_radius
    }

    /// How far the ground is seen from a camera at `altitude` Voxels, in Voxels: the tangent
    /// length to the sphere, `sqrt(h (2R + h))`, plus the same for the relief's peaks.
    pub fn horizon(&self, altitude: f64) -> f64 {
        let tangent = |h: f64| (h.max(0.0) * (2.0 * self.planet_radius + h.max(0.0))).sqrt();
        tangent(altitude) + tangent(self.relief)
    }

    /// How far the shell reaches from a camera at `altitude`: the horizon with the radial margin.
    pub fn shell_reach(&self, altitude: f64) -> f64 {
        self.radial_margin * self.horizon(altitude)
    }

    /// Whether the brick at `coord` of `level` has any of the surface in it: its box meets the
    /// shell of `relief` either side of the sphere.
    pub fn is_surface(&self, level: usize, coord: [i32; 3]) -> bool {
        let size = (8u64 << level) as f64;
        let (mut near, mut far) = (0.0, 0.0);
        for c in coord {
            let low = f64::from(c) * size;
            let nearest = if low > 0.0 {
                low
            } else if low + size < 0.0 {
                -(low + size)
            } else {
                0.0
            };
            let farthest = low.abs().max((low + size).abs());
            near += nearest * nearest;
            far += farthest * farthest;
        }
        let thick = self.relief + self.crust * size;
        near.sqrt() <= self.planet_radius + thick && far.sqrt() >= self.planet_radius - thick
    }
}

pub fn norm(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    a.map(|c| c * s)
}

pub fn unit(a: [f64; 3]) -> [f64; 3] {
    scale(a, 1.0 / norm(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_is_the_tangent_length() {
        let model = Model::real();
        // 2 m up, a 16 km sphere: sqrt(2 · 16000 · 2) = 253 m, the research's figure.
        let metres = model.horizon(EYE) / VOXELS_PER_METRE;
        assert!((metres - 253.0).abs() < 1.0, "{metres}");
        // The margin is 1.25 of that: 316 m.
        let reach = model.shell_reach(EYE) / VOXELS_PER_METRE;
        assert!((reach - 316.0).abs() < 1.5, "{reach}");
        // Below the ground there is no horizon.
        assert_eq!(model.horizon(-5.0), 0.0);
    }

    #[test]
    fn a_brick_is_on_the_surface_if_it_meets_the_sphere() {
        let mut model = Model::real();
        model.crust = 0.0;
        model.planet_radius = 100.0;
        // Level 0 bricks are 8 Voxels: the one at x = 12 spans 96 to 104.
        assert!(model.is_surface(0, [12, 0, 0]));
        assert!(!model.is_surface(0, [11, 0, 0]), "wholly inside");
        assert!(!model.is_surface(0, [13, 0, 0]), "wholly outside");
        // A level 3 brick (64 across) at the origin reaches 111 away at its corner but holds
        // the centre, so it meets the sphere; the one at x = 1 spans 64..128 and does too.
        assert!(model.is_surface(3, [0, 0, 0]));
        assert!(model.is_surface(3, [1, 0, 0]));
        // One wholly beyond is not.
        assert!(!model.is_surface(3, [2, 0, 0]));
        // Relief thickens the surface.
        model.relief = 8.0;
        assert!(model.is_surface(0, [11, 0, 0]));
        assert!(model.is_surface(0, [13, 0, 0]));
    }
}
