//! Camera paths: where the camera is, which way it looks and how it is moving, once per
//! simulated frame. Positions are in Voxels about the Planet's centre, which is the origin.

use crate::model::{EYE, Model, VOXELS_PER_METRE, add, cross, dot, scale, sub, unit};

/// Where the paths start: a direction from the Planet's centre that no brick grid lines up with.
const START: [f64; 3] = [0.36, 0.48, 0.8];

/// One frame of a path.
#[derive(Clone, Debug)]
pub struct Sample {
    pub pos: [f64; 3],
    /// The view's forward, right and up unit vectors.
    pub fwd: [f64; 3],
    pub right: [f64; 3],
    pub up: [f64; 3],
    /// What the prefetcher knows of the camera's motion, in Voxels per second. Zero after a
    /// teleport, which nothing could have predicted.
    pub vel: [f64; 3],
    /// Where the camera is going to be, if its controller knows (a Zoom's does): prefetch builds
    /// the shell at these places instead of extrapolating the velocity.
    pub ahead: Vec<[f64; 3]>,
}

#[derive(Clone, Debug)]
pub struct Path {
    pub name: String,
    /// Seconds between frames.
    pub dt: f64,
    pub samples: Vec<Sample>,
    /// Seconds of soft misses that nothing could avoid: a teleport has no warning, so the shell
    /// at its destination fills in after it (the Guard covers the gap). Zero for the others.
    pub allowed_soft_seconds: f64,
}

/// A sample at `pos`, heading along the ground towards `heading` and looking `pitch` radians
/// above the horizon.
fn sample(pos: [f64; 3], heading: [f64; 3], pitch: f64, vel: [f64; 3]) -> Sample {
    let up = unit(pos);
    let along = sub(heading, scale(up, dot(heading, up)));
    let ahead = unit(along);
    let right = cross(ahead, up);
    let fwd = add(scale(ahead, pitch.cos()), scale(up, pitch.sin()));
    Sample {
        pos,
        fwd,
        right,
        up: cross(right, fwd),
        vel,
        ahead: Vec::new(),
    }
}

fn start(model: &Model, altitude: f64) -> ([f64; 3], [f64; 3]) {
    let up = unit(START);
    let (ahead, _) = planet_gen::planet::tangents(up);
    (scale(up, model.planet_radius + altitude), ahead)
}

fn frames(seconds: f64, dt: f64) -> usize {
    (seconds / dt).round() as usize + 1
}

/// Seconds that flight and take-off take to reach their speed from rest. The client's free-fly
/// (`bins/client/src/fly.rs`) has no acceleration (it moves at full speed on the first frame), so
/// this is a realistic ramp, not the client's: constant acceleration over one second (50 m/s²
/// for 50 m/s). A path that stepped to its speed would charge prefetch for a jump that no
/// controller makes.
pub const RAMP_SECONDS: f64 = 1.0;

/// How far a camera that accelerates uniformly from rest to `v` over [`RAMP_SECONDS`], then holds
/// `v`, has gone at `t`, and how fast it is going.
fn travel(t: f64, v: f64) -> (f64, f64) {
    if t < RAMP_SECONDS {
        (0.5 * v * t * t / RAMP_SECONDS, v * t / RAMP_SECONDS)
    } else {
        (0.5 * v * RAMP_SECONDS + v * (t - RAMP_SECONDS), v)
    }
}

/// Flight along the ground, a great circle, at `speed` m/s and `altitude` m, looking ahead,
/// accelerating from rest ([`RAMP_SECONDS`]).
pub fn flight(model: &Model, altitude: f64, speed: f64, seconds: f64, dt: f64) -> Path {
    let radius = model.planet_radius + altitude * VOXELS_PER_METRE;
    let (origin, ahead) = start(model, altitude * VOXELS_PER_METRE);
    let up0 = unit(origin);
    let v = speed * VOXELS_PER_METRE;
    let samples = (0..frames(seconds, dt))
        .map(|i| {
            let (distance, now) = travel(i as f64 * dt, v);
            let angle = distance / radius;
            let up = add(scale(up0, angle.cos()), scale(ahead, angle.sin()));
            let heading = sub(scale(ahead, angle.cos()), scale(up0, angle.sin()));
            sample(scale(up, radius), heading, 0.0, scale(heading, now))
        })
        .collect();
    Path {
        name: format!("Flight {speed} m/s at {altitude} m"),
        dt,
        samples,
        allowed_soft_seconds: 0.0,
    }
}

/// A turn in place at the ground's eye height, `degrees` per second.
pub fn turn(model: &Model, degrees: f64, seconds: f64, dt: f64) -> Path {
    let (origin, ahead) = start(model, EYE);
    let up = unit(origin);
    let side = cross(up, ahead);
    let rate = degrees.to_radians();
    let samples = (0..frames(seconds, dt))
        .map(|i| {
            let angle = rate * i as f64 * dt;
            let heading = add(scale(ahead, angle.cos()), scale(side, angle.sin()));
            sample(origin, heading, 0.0, [0.0; 3])
        })
        .collect();
    Path {
        name: format!("Turn {degrees} deg/s"),
        dt,
        samples,
        allowed_soft_seconds: 0.0,
    }
}

/// A vertical take-off from the ground's eye height at `speed` m/s, looking at the horizon,
/// accelerating from rest ([`RAMP_SECONDS`]).
pub fn takeoff(model: &Model, speed: f64, seconds: f64, dt: f64) -> Path {
    let (origin, ahead) = start(model, EYE);
    let up = unit(origin);
    let v = speed * VOXELS_PER_METRE;
    let samples = (0..frames(seconds, dt))
        .map(|i| {
            let (height, now) = travel(i as f64 * dt, v);
            let pos = add(origin, scale(up, height));
            sample(pos, ahead, 0.0, scale(up, now))
        })
        .collect();
    Path {
        name: format!("Take-off {speed} m/s vertical"),
        dt,
        samples,
        allowed_soft_seconds: 0.0,
    }
}

/// Most places along its way that a Zoom prefetches: one for each octave of altitude that the
/// next lead covers (a level of the shell is an octave of distance), so that the shell is there
/// at every altitude the Zoom passes through, not only at its end.
pub const MAX_LOOKAHEAD_POINTS: usize = 12;

/// A Zoom straight up or down between two altitudes (in Voxels) over `seconds`, eased in the
/// logarithm of the altitude so that every octave takes about as long. The camera looks at the
/// Planet from orbit and at the horizon from the ground. The Zoom's controller knows where it
/// is going, so the places it will be in the next `model.lead` seconds are its `ahead`: one for each
/// octave of altitude, at even steps of its logarithm, the last at the end of the lead.
pub fn zoom(model: &Model, from: f64, to: f64, seconds: f64, dt: f64) -> Path {
    let (_, heading) = start(model, 0.0);
    let up = unit(START);
    let (low, high) = (500.0f64, 1.0e6f64);
    let span = to.ln() - from.ln();
    let altitude_at = |time: f64| {
        let s = (time / seconds).clamp(0.0, 1.0);
        (from.ln() + span * s * s * (3.0 - 2.0 * s)).exp()
    };
    let at = |time: f64| {
        let s = (time / seconds).clamp(0.0, 1.0);
        let altitude = altitude_at(time);
        let vel = altitude * span * 6.0 * s * (1.0 - s) / seconds;
        let pitch = -std::f64::consts::FRAC_PI_2
            * ((altitude / low).ln() / (high / low).ln()).clamp(0.0, 1.0);
        let mut sample = sample(
            scale(up, model.planet_radius + altitude),
            heading,
            pitch,
            scale(up, vel),
        );
        if let Some((_, lead)) = model.lead {
            let end = altitude_at(time + lead).ln();
            let change = end - altitude.ln();
            let n = ((change.abs() / std::f64::consts::LN_2).ceil() as usize)
                .clamp(1, MAX_LOOKAHEAD_POINTS);
            sample.ahead = (1..=n)
                .map(|k| {
                    let ln = altitude.ln() + change * k as f64 / n as f64;
                    scale(up, model.planet_radius + ln.exp())
                })
                .collect();
        }
        sample
    };
    let samples = (0..frames(seconds, dt))
        .map(|i| at(i as f64 * dt))
        .collect();
    let name = if to > from {
        format!(
            "Zoom out to {:.0} R in {seconds} s",
            (model.planet_radius + to) / model.planet_radius
        )
    } else {
        format!(
            "Zoom in from {:.0} R in {seconds} s",
            (model.planet_radius + from) / model.planet_radius
        )
    };
    Path {
        name,
        dt,
        samples,
        allowed_soft_seconds: 0.0,
    }
}

/// A teleport at ground level, `distance` m along the ground, `before` seconds in, with `after`
/// seconds to run on. Nothing says it is coming, so nothing is prefetched.
pub fn teleport(model: &Model, distance: f64, before: f64, after: f64, dt: f64) -> Path {
    let radius = model.planet_radius + EYE;
    let (origin, ahead) = start(model, EYE);
    let up0 = unit(origin);
    let angle = distance * VOXELS_PER_METRE / radius;
    let there = add(scale(up0, angle.cos()), scale(ahead, angle.sin()));
    let n_before = frames(before, dt);
    let samples = (0..n_before + frames(after, dt) - 1)
        .map(|i| {
            let pos = if i < n_before { up0 } else { there };
            sample(scale(pos, radius), ahead, 0.0, [0.0; 3])
        })
        .collect();
    Path {
        name: format!("Teleport {distance} m"),
        dt,
        samples,
        // The shell at the destination is generated after the fact: allow it a second.
        allowed_soft_seconds: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::norm;

    #[test]
    fn flight_accelerates_from_rest_to_its_speed_and_keeps_its_altitude() {
        let model = Model::real();
        let path = flight(&model, 50.0, 50.0, 4.0, 0.1);
        assert_eq!(path.samples.len(), 41);
        let (a, b) = (&path.samples[0], &path.samples[40]);
        assert_eq!(norm(a.vel), 0.0, "it starts at rest");
        assert!(
            (norm(path.samples[5].vel) - 250.0).abs() < 1e-6,
            "half way up the ramp"
        );
        // 1 s of ramp (25 m) then 3 s at 50 m/s: 175 m, 1750 Voxels, which the chord
        // all but equals.
        let chord = norm(sub(b.pos, a.pos));
        assert!((chord - 1750.0).abs() < 1.0, "{chord}");
        for (i, s) in path.samples.iter().enumerate() {
            assert!((model.altitude(s.pos) - 500.0).abs() < 1e-6);
            if i >= 10 {
                assert!((norm(s.vel) - 500.0).abs() < 1e-6);
            }
            assert!(
                dot(s.fwd, unit(s.pos)).abs() < 1e-9,
                "level flight looks at the horizon"
            );
        }
    }

    #[test]
    fn a_turn_stays_put_and_sweeps_its_rate() {
        let model = Model::real();
        let path = turn(&model, 180.0, 1.0, 0.1);
        let (a, b) = (&path.samples[0], &path.samples[5]);
        assert_eq!(a.pos, b.pos);
        // Half a second at 180 degrees a second: a quarter turn.
        assert!(dot(a.fwd, b.fwd).abs() < 1e-9, "{}", dot(a.fwd, b.fwd));
    }

    #[test]
    fn a_takeoff_climbs_and_a_zoom_ends_where_it_says() {
        let model = Model::real();
        let up = takeoff(&model, 50.0, 2.0, 0.1);
        let last = up.samples.last().unwrap();
        // 25 m of ramp and then 1 s at 50 m/s: 75 m.
        assert!((model.altitude(last.pos) - (EYE + 750.0)).abs() < 1e-6);
        let zoom = zoom(&model, EYE, 7.0e6, 5.0, 0.1);
        let first = &zoom.samples[0];
        let last = zoom.samples.last().unwrap();
        assert!((model.altitude(first.pos) - EYE).abs() < 1e-6);
        assert!((model.altitude(last.pos) - 7.0e6).abs() < 1.0);
        assert!(
            zoom.samples
                .iter()
                .all(|s| (1..=MAX_LOOKAHEAD_POINTS).contains(&s.ahead.len()))
        );
        // Looking down at orbit, along the horizon at the ground.
        assert!(dot(last.fwd, unit(last.pos)) < -0.99);
        assert!(dot(first.fwd, unit(first.pos)).abs() < 0.01);
    }

    #[test]
    fn a_teleport_jumps_once_with_no_velocity() {
        let model = Model::real();
        let path = teleport(&model, 8000.0, 1.0, 2.0, 0.1);
        let jumps = path
            .samples
            .windows(2)
            .filter(|w| norm(sub(w[1].pos, w[0].pos)) > 1.0)
            .count();
        assert_eq!(jumps, 1);
        assert!(path.samples.iter().all(|s| norm(s.vel) == 0.0));
        assert_eq!(path.samples.len(), 31);
    }
}
