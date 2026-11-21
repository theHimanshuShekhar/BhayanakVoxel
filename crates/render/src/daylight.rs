//! The day and the night (#203): where the sun and the moon are at a time of day, how dark the sky
//! is, and how the sun's moving is told from the sun's being put somewhere else.
//!
//! The time of day is a fraction of a day in `[0, 1)`, as `sim::WorldClock` keeps it (0 is
//! midnight, 0.25 sunrise, 0.5 noon, 0.75 sunset). This module has no clock in it: [`DaySky::at`] is
//! a function of the fraction alone, so the same time is the same sky, which is what a golden of a
//! time of day holds.
//!
//! **The sky is in the camera's local frame**, as the game's sun already was ([`Sun::local`]): `x` is
//! to the camera's side, `y` its local up (the Planet's radial up over a Planet) and `z` its `back`.
//! The time of day is the same everywhere on the Planet, and every view of it sees the sun, the moon
//! and the stars from its own up. [`LocalFrame`] turns a vector of that frame into the world's.
//!
//! **The sun's path** is a circle in the sky tilted so that the sun reaches [`MAX_ELEVATION`] at noon.
//! It is fitted so that at [`hero_time_of_day`] the sun is exactly where hero light B puts it
//! ([`crate::sun::DEFAULT_AZIMUTH`] and [`crate::sun::DEFAULT_ELEVATION`]) on its way down in the
//! evening: a clock set to that time gives the look the owner picked in #148.
//!
//! **The moon** is on the same circle, [`MOON_PHASE_OFFSET`] ahead of the point opposite the sun, so
//! that it is up from a little before sunset to a little before sunrise: always a full moon (it is
//! a disc in the sky, not a place). The stars turn with the sky about the circle's axis.
//!
//! **Jump and drift.** A running clock moves the sun a little every frame, and a clock that is set
//! (or a step of many minutes) puts it somewhere else. The light's caches follow the first and must
//! forget at once for the second, so [`Daylight`] says which a step was. See [`Daylight::step`].

use crate::camera::Camera;
use crate::sun::{DEFAULT_ANGULAR_RADIUS, DEFAULT_AZIMUTH, DEFAULT_ELEVATION, Sun};

/// The sun's elevation at noon, in degrees (above the afternoon's 28 degrees and the 60 of the
/// tint's midday setting, see `atmosphere::Tint`).
pub const MAX_ELEVATION: f64 = 65.0;

/// How far along the sky's circle, in degrees, the moon is ahead of the point opposite the sun: it
/// rises 15 degrees of the sky's turn (a 24th of a day) before the sun sets.
pub const MOON_PHASE_OFFSET: f64 = 15.0;

/// Half the moon's angular size in radians: the radius of its disc in the sky and of the light it
/// casts. About 3.4 degrees, a big, soft moon (twelve times the real one's), as the sun is.
pub const MOON_ANGULAR_RADIUS: f32 = 0.06;

/// The moon's light on a surface that faces it above the air, as a multiple of the surface's
/// colour (the sun's `Atmosphere::sun_light` is the same kind of number): dim and cool, a twentieth
/// of the sun's, which the exposure then lifts.
pub const MOON_LIGHT: [f32; 3] = [0.018, 0.020, 0.038];

/// The moon's disc as the sky shows it: its radiance (the units of the sky's own), pale and cool.
/// The disc is added to the sky and the light it stands for is the key light's, so nothing counts it
/// twice (see `tracer::Key`).
pub const MOON_DISC: [f32; 3] = [0.30, 0.29, 0.38];

/// How far the sun's step in one frame may go, in radians, and still be a drift: 2 degrees. A
/// clock at the default day moves the sun 0.003 degrees a frame; a sped-up cycle of a minute 0.1.
pub const JUMP_ANGLE: f32 = 2.0 * std::f32::consts::PI / 180.0;

const DEGREES: f64 = std::f64::consts::PI / 180.0;

/// The circle the sun and the moon go along, in the local frame.
struct Orbit {
    /// The point of the circle at angle 0 (sunrise), a unit vector on the horizon.
    rise: [f64; 3],
    /// The point at a quarter of the way round (noon).
    noon: [f64; 3],
}

impl Orbit {
    fn new() -> Self {
        let (sin_e, cos_e) = (MAX_ELEVATION * DEGREES).sin_cos();
        // On the way down in the evening the sun is at the hero elevation when the angle round the
        // circle is pi - asin(sin(hero) / sin(max)); its horizontal part is then
        // -cos(a) rise + sin(a) cos(max) across, where `across` is `rise` turned a quarter of a turn
        // towards -azimuth. Solve for the azimuth of `rise` that puts that at the hero azimuth.
        let hero = f64::from(DEFAULT_ELEVATION);
        let s0 = hero.sin() / sin_e;
        let c0 = (1.0 - s0 * s0).sqrt();
        let lean = (s0 * cos_e).atan2(c0);
        let rise_azimuth = f64::from(DEFAULT_AZIMUTH) - std::f64::consts::PI + lean;
        let at = |azimuth: f64| [azimuth.sin(), 0.0, azimuth.cos()];
        let rise = at(rise_azimuth);
        let across = at(rise_azimuth + std::f64::consts::FRAC_PI_2);
        let noon = [cos_e * across[0], sin_e, cos_e * across[2]];
        Self { rise, noon }
    }

    /// The point of the circle at `angle` radians (0 sunrise, a quarter noon, a half sunset).
    fn at(&self, angle: f64) -> [f64; 3] {
        let (s, c) = angle.sin_cos();
        std::array::from_fn(|a| c * self.rise[a] + s * self.noon[a])
    }

    /// The circle's axis: the vector that the sky turns about.
    fn axis(&self) -> [f64; 3] {
        cross(self.rise, self.noon)
    }
}

/// The angle round the circle at a time of day: 0 at sunrise.
fn angle_of(time_of_day: f64) -> f64 {
    (time_of_day - 0.25) * std::f64::consts::TAU
}

/// The time of day at which the sun is where hero light B has it (8 degrees up, at azimuth 110),
/// on its way down in the evening: a golden hour a little before sunset.
pub fn hero_time_of_day() -> f64 {
    let sin_e = (MAX_ELEVATION * DEGREES).sin();
    0.75 - (f64::from(DEFAULT_ELEVATION).sin() / sin_e).asin() / std::f64::consts::TAU
}

/// The sky at a time of day, in the local frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DaySky {
    /// The time of day it is the sky of, in `[0, 1)`.
    pub time_of_day: f64,
    /// Unit vectors to the sun and to the moon.
    pub sun: [f32; 3],
    pub moon: [f32; 3],
    /// How dark the sky is, 0 (day) to 1 (night): the share of the night sky's own light (the
    /// lilac floor that the atmosphere adds, `atmosphere::Atmosphere::night`) that is there. It
    /// follows the sun's elevation, and is 0 while any of the sun's light is in the air.
    pub night: f32,
    /// How visible the stars are, 0 to 1: they come out as the twilight goes.
    pub stars: f32,
    /// How visible the moon's disc is, 0 to 1: pale at sunset, gone in the daylight.
    pub moon_visible: f32,
    /// The sky's turn since sunrise, in radians, about the axis of the sun's circle: where the
    /// stars are.
    pub turn: f32,
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl DaySky {
    /// The sky at `time_of_day` (wrapped into `[0, 1)`).
    pub fn at(time_of_day: f64) -> DaySky {
        let t = if time_of_day.is_finite() {
            time_of_day.rem_euclid(1.0)
        } else {
            0.0
        };
        let orbit = Orbit::new();
        let angle = angle_of(t);
        let sun = orbit.at(angle).map(|c| c as f32);
        let moon = orbit
            .at(angle + std::f64::consts::PI + MOON_PHASE_OFFSET * DEGREES)
            .map(|c| c as f32);
        let elevation = sun[1].clamp(-1.0, 1.0).asin().to_degrees();
        DaySky {
            time_of_day: t,
            sun,
            moon,
            // The tint is at its night setting by -8 degrees, and the physical sky still holds the
            // sun's glow to -40: the floor comes in under it as the glow goes, from -4 to -24.
            night: 1.0 - smoothstep(-24.0, -4.0, elevation),
            stars: 1.0 - smoothstep(-18.0, -6.0, elevation),
            moon_visible: 1.0 - smoothstep(0.0, 30.0, elevation),
            turn: angle as f32,
        }
    }

    /// The sun's elevation above the local horizon, in degrees.
    pub fn sun_elevation_degrees(&self) -> f32 {
        self.sun[1].clamp(-1.0, 1.0).asin().to_degrees()
    }

    /// The moon's elevation above the local horizon, in degrees.
    pub fn moon_elevation_degrees(&self) -> f32 {
        self.moon[1].clamp(-1.0, 1.0).asin().to_degrees()
    }

    /// The matrix (three rows) from the local frame's vectors to the stars' own: the stars are
    /// fixed in the sky that turns about the circle's axis, so a direction of the local frame is
    /// turned back by the sky's turn to find the star there.
    fn star_rows(&self) -> [[f32; 3]; 3] {
        let axis = unit(Orbit::new().axis());
        rotation_rows(axis, -f64::from(self.turn))
    }
}

/// Rows of the rotation by `angle` radians about the unit `axis` (Rodrigues).
fn rotation_rows(axis: [f64; 3], angle: f64) -> [[f32; 3]; 3] {
    let (s, c) = angle.sin_cos();
    let [x, y, z] = axis;
    let t = 1.0 - c;
    [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ]
    .map(|row| row.map(|v| v as f32))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let l = a.iter().map(|c| c * c).sum::<f64>().sqrt();
    a.map(|c| c / l)
}

/// The frame that the sky's directions are in: the vectors the local `x`, `y` and `z` are in the
/// world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalFrame {
    pub side: [f64; 3],
    pub up: [f64; 3],
    pub back: [f64; 3],
}

impl LocalFrame {
    /// `camera`'s frame: its local up, its zero-yaw `back` and the side that [`Sun::local`] uses.
    pub fn of(camera: &Camera) -> Self {
        let (up, back) = (camera.horizon_up, camera.back);
        Self {
            side: cross(up, back),
            up,
            back,
        }
    }

    /// The world's vector for the local one `v`.
    pub fn to_world(&self, v: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|a| {
            (self.side[a] * f64::from(v[0])
                + self.up[a] * f64::from(v[1])
                + self.back[a] * f64::from(v[2])) as f32
        })
    }

    /// The local vector of the world's `w`: the inverse of [`LocalFrame::to_world`].
    pub fn to_local(&self, w: [f32; 3]) -> [f32; 3] {
        let dot = |axis: [f64; 3]| (0..3).map(|a| axis[a] * f64::from(w[a])).sum::<f64>() as f32;
        [dot(self.side), dot(self.up), dot(self.back)]
    }
}

/// The moon as the views have it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moon {
    /// The unit vector towards the moon.
    pub direction: [f32; 3],
    /// Half the angle its disc spans, in radians; the light's penumbra is the same width.
    pub angular_radius: f32,
    /// The light above the air (see [`MOON_LIGHT`]).
    pub light: [f32; 3],
    /// How visible the disc is, 0 to 1.
    pub visible: f32,
}

/// What the night adds to the sky, in the world's directions: the moon, the lilac floor and the
/// stars. [`Night::NONE`] is a sky with none of it, which is every sky the renderer drew before
/// #203: nothing of the night is read then, and a frame is the one it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Night {
    pub moon: Option<Moon>,
    /// How much of the night sky's own light there is, 0 to 1 (see [`DaySky::night`]).
    pub level: f32,
    /// How visible the stars are, 0 to 1.
    pub stars: f32,
    /// The rows of the matrix from a world direction to the stars' own.
    pub star_rows: [[f32; 3]; 3],
}

impl Night {
    /// No night: no moon, no floor, no stars.
    pub const NONE: Night = Night {
        moon: None,
        level: 0.0,
        stars: 0.0,
        star_rows: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
}

impl DaySky {
    /// The sun of this sky in the world, with the default angular size.
    pub fn sun_in(&self, frame: &LocalFrame) -> Sun {
        Sun::new(frame.to_world(self.sun), DEFAULT_ANGULAR_RADIUS).expect("a unit vector")
    }

    /// The night of this sky in the world.
    pub fn night_in(&self, frame: &LocalFrame) -> Night {
        let local = self.star_rows();
        // The stars' matrix takes a local vector; to take a world one, it is applied after the
        // frame's own inverse (the rows of the local axes).
        let inverse = [frame.side, frame.up, frame.back];
        let rows = std::array::from_fn(|r| {
            std::array::from_fn(|c| {
                (0..3)
                    .map(|k| f64::from(local[r][k]) * inverse[k][c])
                    .sum::<f64>() as f32
            })
        });
        Night {
            moon: Some(Moon {
                direction: frame.to_world(self.moon),
                angular_radius: MOON_ANGULAR_RADIUS,
                light: MOON_LIGHT,
                visible: self.moon_visible,
            }),
            level: self.night,
            stars: self.stars,
            star_rows: rows,
        }
    }
}

/// What a step of the sky was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drive {
    pub sky: DaySky,
    /// The sun was **put** somewhere (the clock was set, a step took it far, or this is the first
    /// sky), and did not move on from where it was. A jump makes the radiance cache forget at once
    /// and the history of the denoiser and the upscaler start again; the slow drift of a running
    /// clock does neither, and is followed by the caches' normal updates.
    pub jumped: bool,
}

/// Tells a step of the sky that is a drift from one that is a jump.
#[derive(Clone, Debug, Default)]
pub struct Daylight {
    last: Option<(DaySky, u32)>,
}

impl Daylight {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sky at `time_of_day` for the clock whose `sim::WorldClock::epoch` is `epoch`, and whether it is a
    /// jump from the sky of the last step. It is a jump if
    /// - it is the first step;
    /// - the clock was set since the last (its epoch moved), whatever the time was set to; or
    /// - the sun went further than [`JUMP_ANGLE`] in the step (a big step of a fast or a
    ///   long-stalled clock). The angle is between the two sun directions, so a time that wraps
    ///   past midnight is a small step and a whole day is none.
    ///
    /// A slower step is a drift. The time of day alone does not say which: the same two times are a
    /// drift for a clock that ran and a jump for one that was set.
    pub fn step(&mut self, time_of_day: f64, epoch: u32) -> Drive {
        let sky = DaySky::at(time_of_day);
        let jumped = match &self.last {
            None => true,
            Some((last, last_epoch)) => {
                *last_epoch != epoch || angle_between(last.sun, sky.sun) > JUMP_ANGLE
            }
        };
        self.last = Some((sky, epoch));
        Drive { sky, jumped }
    }
}

/// The angle between two unit vectors, in radians.
pub fn angle_between(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dot: f32 = (0..3).map(|i| a[i] * b[i]).sum();
    // The chord is exact for small angles where the cosine is not.
    let chord = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    if dot > 0.0 {
        2.0 * (chord / 2.0).clamp(0.0, 1.0).asin()
    } else {
        dot.clamp(-1.0, 1.0).acos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < tolerance)
    }

    #[test]
    fn at_the_hero_time_the_sun_is_where_hero_light_b_has_it() {
        let sky = DaySky::at(hero_time_of_day());
        let hero = Sun::default().direction();
        assert!(near(sky.sun, hero, 1e-5), "{:?} {hero:?}", sky.sun);
        assert!((sky.sun_elevation_degrees() - 8.0).abs() < 1e-3);
        // On its way down: the hero time is between noon and sunset.
        assert!((0.5..0.75).contains(&hero_time_of_day()));
    }

    #[test]
    fn the_sun_rises_at_a_quarter_peaks_at_noon_and_sets_at_three_quarters() {
        let elevation = |t| DaySky::at(t).sun_elevation_degrees();
        assert!(elevation(0.25).abs() < 1e-3);
        assert!((elevation(0.5) - 65.0).abs() < 1e-3);
        assert!(elevation(0.75).abs() < 1e-3);
        assert!((elevation(0.0) + 65.0).abs() < 1e-3, "midnight is deepest");
        assert!(elevation(0.4) > 0.0 && elevation(0.6) > 0.0);
        assert!(elevation(0.1) < 0.0 && elevation(0.9) < 0.0);
    }

    #[test]
    fn the_sun_is_up_for_half_the_day() {
        let up = (0..1000)
            .filter(|n| DaySky::at(f64::from(*n) / 1000.0).sun[1] > 0.0)
            .count();
        assert!((499..=501).contains(&up), "{up}");
    }

    #[test]
    fn a_time_of_day_is_a_sky_and_a_day_later_is_the_same_one() {
        assert_eq!(DaySky::at(0.3), DaySky::at(0.3));
        let a = DaySky::at(0.3);
        let b = DaySky::at(1.3);
        assert!(near(a.sun, b.sun, 1e-5) && near(a.moon, b.moon, 1e-5));
        assert!((a.night - b.night).abs() < 1e-6);
        assert!(near(DaySky::at(-0.1).sun, DaySky::at(0.9).sun, 1e-5));
        // A time that is not a number is midnight.
        assert_eq!(DaySky::at(f64::NAN), DaySky::at(0.0));
    }

    #[test]
    fn the_directions_are_unit_vectors() {
        for n in 0..200 {
            let sky = DaySky::at(f64::from(n) / 200.0);
            for v in [sky.sun, sky.moon] {
                let l: f32 = v.iter().map(|c| c * c).sum::<f32>().sqrt();
                assert!((l - 1.0).abs() < 1e-5, "{l}");
            }
        }
    }

    #[test]
    fn the_moon_is_up_from_before_sunset_to_before_sunrise() {
        let moon = |t| DaySky::at(t).moon_elevation_degrees();
        // At sunset it has risen; at midnight it is as high as the sun is at noon (the offset
        // takes it a little off the sun's opposite point); at sunrise it has set.
        assert!(moon(0.75) > 5.0, "{}", moon(0.75));
        assert!(moon(0.0) > 55.0, "{}", moon(0.0));
        assert!(moon(0.25) < -5.0, "{}", moon(0.25));
        // Up for as long as the sun is down.
        let up = (0..1000)
            .filter(|n| DaySky::at(f64::from(*n) / 1000.0).moon[1] > 0.0)
            .count();
        assert!((499..=501).contains(&up), "{up}");
        // Never at the sun's place: a disc of its own in the sky.
        for n in 0..200 {
            let sky = DaySky::at(f64::from(n) / 200.0);
            assert!(angle_between(sky.sun, sky.moon) > 2.5, "{n}");
        }
    }

    #[test]
    fn the_night_is_dark_at_midnight_and_gone_by_the_afternoon() {
        let midnight = DaySky::at(0.0);
        assert_eq!((midnight.night, midnight.stars), (1.0, 1.0));
        let noon = DaySky::at(0.5);
        assert_eq!((noon.night, noon.stars, noon.moon_visible), (0.0, 0.0, 0.0));
        let hero = DaySky::at(hero_time_of_day());
        assert_eq!((hero.night, hero.stars), (0.0, 0.0));
        // The stars come out after the floor begins and while the sun is under the horizon.
        let dusk = DaySky::at(0.75 + 0.01);
        assert!(dusk.sun_elevation_degrees() < 0.0);
        assert!(dusk.night < 0.2 && dusk.stars < 0.01, "{dusk:?}");
        // The moon is faintly there at sunset and fully when it is dark.
        assert!(DaySky::at(0.75).moon_visible > 0.9);
        assert_eq!(midnight.moon_visible, 1.0);
    }

    #[test]
    fn the_night_comes_on_smoothly_with_the_sun_going_down() {
        // No step of a thousandth of a day changes any level by more than a few hundredths.
        let mut last = DaySky::at(0.0);
        for n in 1..=1000 {
            let sky = DaySky::at(f64::from(n) / 1000.0);
            for (a, b) in [
                (last.night, sky.night),
                (last.stars, sky.stars),
                (last.moon_visible, sky.moon_visible),
            ] {
                assert!((a - b).abs() < 0.06, "{n}: {a} {b}");
            }
            last = sky;
        }
    }

    #[test]
    fn the_sky_turns_the_stars_with_the_sun_about_the_circles_axis() {
        // A star direction that the matrix finds for the sun's direction at one time is the one
        // for it a quarter-day later at the sun's position then.
        let sky = DaySky::at(0.3);
        let later = DaySky::at(0.55);
        let apply = |rows: [[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] {
            std::array::from_fn(|r| (0..3).map(|c| rows[r][c] * v[c]).sum())
        };
        let star = apply(sky.star_rows(), sky.sun);
        let star_later = apply(later.star_rows(), later.sun);
        assert!(near(star, star_later, 1e-5), "{star:?} {star_later:?}");
        // And a rotation keeps lengths.
        let l: f32 = star.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!((l - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_frame_takes_the_local_sky_to_the_world_and_back() {
        let mut camera = Camera::looking_at([0.0, 5.0, 0.0], [0.0; 3]);
        camera.position = [100.0, 0.0, 0.0];
        camera.carry_up([1.0, 0.0, 0.0]);
        let frame = LocalFrame::of(&camera);
        let sky = DaySky::at(0.3);
        let world = frame.to_world(sky.sun);
        assert!(near(frame.to_local(world), sky.sun, 1e-5));
        // The sun is as high above this camera's horizon as above an uncarried one's.
        let up = camera.horizon_up.map(|c| c as f32);
        let height: f32 = (0..3).map(|i| world[i] * up[i]).sum();
        assert!((height - sky.sun[1]).abs() < 1e-5);
        // The same as `Sun::local` for the angles of the sun.
        let (azimuth, elevation) = Sun::new(sky.sun, 0.1).unwrap().angles();
        let by_angles = Sun::local(&camera, azimuth, elevation, 0.1).direction();
        assert!(near(world, by_angles, 1e-5), "{world:?} {by_angles:?}");
    }

    #[test]
    fn the_stars_of_a_world_direction_are_those_of_its_local_one() {
        let mut camera = Camera::looking_at([0.0, 5.0, 0.0], [0.0; 3]);
        camera.position = [0.0, 0.0, 100.0];
        camera.carry_up([0.0, 0.0, 1.0]);
        let frame = LocalFrame::of(&camera);
        let sky = DaySky::at(0.9);
        let night = sky.night_in(&frame);
        let local = sky.star_rows();
        let v = [0.3, 0.8, -0.5];
        let w = frame.to_world(v);
        let by_world: [f32; 3] =
            std::array::from_fn(|r| (0..3).map(|c| night.star_rows[r][c] * w[c]).sum());
        let by_local: [f32; 3] = std::array::from_fn(|r| (0..3).map(|c| local[r][c] * v[c]).sum());
        assert!(near(by_world, by_local, 1e-5), "{by_world:?} {by_local:?}");
    }

    #[test]
    fn night_none_is_nothing_of_the_night() {
        assert_eq!(Night::NONE.moon, None);
        assert_eq!((Night::NONE.level, Night::NONE.stars), (0.0, 0.0));
    }

    #[test]
    fn the_first_step_is_a_jump_and_a_small_one_after_it_is_a_drift() {
        let mut daylight = Daylight::new();
        assert!(daylight.step(0.3, 0).jumped);
        // A day of 30 minutes at 60 Hz: 1/108000 of a day a frame.
        let mut t = 0.3;
        for _ in 0..600 {
            t += 1.0 / 108_000.0;
            assert!(!daylight.step(t, 0).jumped, "{t}");
        }
    }

    #[test]
    fn setting_the_clock_is_a_jump_whatever_the_time_it_is_set_to() {
        let mut daylight = Daylight::new();
        daylight.step(0.3, 0);
        // Set to where it is: the epoch moved, so the sun was put.
        assert!(daylight.step(0.3, 1).jumped);
        assert!(!daylight.step(0.3, 1).jumped);
        // Set a long way off, and a hair off.
        assert!(daylight.step(0.8, 2).jumped);
        assert!(daylight.step(0.8001, 3).jumped);
    }

    #[test]
    fn a_big_step_of_a_running_clock_is_a_jump_and_a_fast_one_is_not() {
        let mut daylight = Daylight::new();
        daylight.step(0.3, 0);
        // A step of a twentieth of a day moves the sun 18 degrees of its circle: a jump.
        assert!(daylight.step(0.35, 0).jumped);
        // A day of 60 seconds, a frame at 60 Hz: 1/3600 of a day, 0.1 degree: a drift.
        let mut t = 0.35;
        for _ in 0..100 {
            t += 1.0 / 3600.0;
            assert!(!daylight.step(t, 0).jumped);
        }
    }

    #[test]
    fn the_wrap_at_midnight_is_a_drift_and_a_whole_day_is_no_step_at_all() {
        let mut daylight = Daylight::new();
        daylight.step(0.99999, 0);
        assert!(!daylight.step(0.00001, 0).jumped);
        // A running clock that went a whole day between two steps put the sun where it was.
        daylight.step(0.4, 0);
        assert!(!daylight.step(0.4, 0).jumped);
    }

    #[test]
    fn the_jump_angle_is_two_degrees_and_a_step_just_over_it_is_a_jump() {
        // Round the circle one degree of the sky's turn is 1/360 of a day.
        let one_degree = 1.0 / 360.0;
        let mut daylight = Daylight::new();
        daylight.step(0.5, 0);
        assert!(!daylight.step(0.5 + 1.5 * one_degree, 0).jumped);
        daylight.step(0.5, 0);
        assert!(daylight.step(0.5 + 2.5 * one_degree, 0).jumped);
    }

    #[test]
    fn the_angle_between_is_right_for_small_and_large_angles() {
        let a = [0.0, 1.0, 0.0];
        assert!(angle_between(a, a).abs() < 1e-7);
        assert!((angle_between(a, [1.0, 0.0, 0.0]) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert!((angle_between(a, [0.0, -1.0, 0.0]) - std::f32::consts::PI).abs() < 1e-6);
        let tiny = [1e-4f32.sin(), 1e-4f32.cos(), 0.0];
        assert!((angle_between(a, tiny) - 1e-4).abs() < 1e-8);
    }
}
