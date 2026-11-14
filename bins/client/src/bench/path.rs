//! The scripted camera path: keyframes, interpolated by frame index and never by the clock, so
//! that every run, on any machine, shows the GPU exactly the same views.

use render::Camera;

/// Where the camera is at a keyframe and what it looks at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyframe {
    pub position: [f64; 3],
    pub target: [f64; 3],
}

#[derive(Clone, Debug)]
pub struct CameraPath {
    keyframes: Vec<Keyframe>,
    /// Whether the horizon is levelled to the way up where the camera is (away from the Planet's
    /// centre), not to +y. The flight is over the +y pole, where the two are the same.
    radial: bool,
    /// The camera's roll at each keyframe, in radians (`Camera::roll`), or empty for a path that
    /// keeps its horizon level.
    rolls: Vec<f64>,
}

impl CameraPath {
    /// A path through `keyframes`, which needs at least two.
    pub fn new(keyframes: Vec<Keyframe>) -> Self {
        assert!(keyframes.len() >= 2, "a path needs at least two keyframes");
        Self {
            keyframes,
            radial: false,
            rolls: Vec::new(),
        }
    }

    /// The same path with the camera rolled by `rolls[k]` radians at keyframe `k` (positive
    /// leans it to the right, `Camera::roll`) and by a straight blend between: a Dutch tilt in a
    /// canyon, recorded with the debug free camera's roll keys. Needs one roll for each keyframe.
    pub fn rolled(mut self, rolls: Vec<f64>) -> Self {
        assert_eq!(
            rolls.len(),
            self.keyframes.len(),
            "a path needs one roll for each keyframe"
        );
        self.rolls = rolls;
        self
    }

    /// The same path with the camera's horizon levelled to the Planet's up where it is, for a
    /// path that is not over the pole.
    pub fn radial(mut self) -> Self {
        self.radial = true;
        self
    }

    pub fn keyframes(&self) -> usize {
        self.keyframes.len()
    }

    /// The camera at `frame` of a run of `frames`: the first keyframe at frame 0, the last at
    /// `frames - 1`, with the keyframes in between evenly spaced and the camera moving in a
    /// straight line, at constant speed, from each to the next.
    pub fn camera_at(&self, frame: u32, frames: u32) -> Camera {
        let segments = self.keyframes.len() - 1;
        let last = frames.saturating_sub(1);
        // How far along the whole path, in segments. Integers until the one division, so that
        // frames that land on a keyframe do so exactly.
        let along = if last == 0 {
            0.0
        } else {
            (u64::from(frame.min(last)) * segments as u64) as f64 / f64::from(last)
        };
        let segment = (along.floor() as usize).min(segments - 1);
        let t = along - segment as f64;
        let (from, to) = (&self.keyframes[segment], &self.keyframes[segment + 1]);
        // Written so that t = 0 and t = 1 give exactly `from` and `to`.
        let lerp = |a: [f64; 3], b: [f64; 3]| [0, 1, 2].map(|i| a[i] * (1.0 - t) + b[i] * t);
        let (position, target) = (
            lerp(from.position, to.position),
            lerp(from.target, to.target),
        );
        let mut camera = if self.radial {
            level_camera(position, target)
        } else {
            Camera::looking_at(position, target)
        };
        if let (Some(&a), Some(&b)) = (self.rolls.get(segment), self.rolls.get(segment + 1)) {
            camera.roll = a * (1.0 - t) + b * t;
        }
        camera
    }

    /// The frames of a run of `frames` at which the camera is on a keyframe: the fixed points
    /// where screenshots are taken. Keyframes that fall on the same frame count once.
    pub fn keyframe_frames(&self, frames: u32) -> Vec<u32> {
        let last = f64::from(frames.saturating_sub(1));
        let segments = (self.keyframes.len() - 1) as f64;
        let mut at: Vec<u32> = (0..self.keyframes.len())
            .map(|k| (k as f64 * last / segments).round() as u32)
            .collect();
        at.dedup();
        at
    }
}

/// A camera at `position` looking at `target`, its horizon level with the Planet there.
fn level_camera(position: [f64; 3], target: [f64; 3]) -> Camera {
    let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f64>();
    let unit = |a: [f64; 3]| {
        let length = dot(a, a).sqrt();
        a.map(|c| c / length)
    };
    let up = unit(position);
    let (back, _) = planet_gen::planet::tangents(up);
    let side = [
        up[1] * back[2] - up[2] * back[1],
        up[2] * back[0] - up[0] * back[2],
        up[0] * back[1] - up[1] * back[0],
    ];
    let ahead = unit(std::array::from_fn(|a| target[a] - position[a]));
    let mut camera = Camera::looking_at(position, target);
    camera.horizon_up = up;
    camera.back = back;
    camera.pitch = dot(ahead, up).clamp(-1.0, 1.0).asin();
    camera.yaw = (-dot(ahead, side)).atan2(-dot(ahead, back));
    camera
}

fn key(position: [f64; 3], target: [f64; 3]) -> Keyframe {
    Keyframe { position, target }
}

/// Low over the ground near the +y pole, looking along it towards the horizon and then across
/// it: the part of the flight that `--short` runs on a patch of the Planet. The camera is above
/// 1548 Voxels, over the highest the ground gets (1500 plus the Planet's relief of 34).
fn low_keyframes() -> [Keyframe; 3] {
    [
        key([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]),
        key([0.0, 1552.0, 0.0], [200.0, 1530.0, -300.0]),
        key([-150.0, 1556.0, -200.0], [-300.0, 1530.0, -360.0]),
    ]
}

/// The full flight: from the orbital view that the client starts in, down to the pole, then
/// the low pass.
pub fn planet_flyover() -> CameraPath {
    let mut keyframes = vec![
        key([0.0, 400.0, 3400.0], [0.0; 3]),
        key([0.0, 1900.0, 1500.0], [0.0, 1500.0, 0.0]),
    ];
    keyframes.extend(low_keyframes());
    CameraPath::new(keyframes)
}

/// The low pass alone.
pub fn patch_flyover() -> CameraPath {
    CameraPath::new(low_keyframes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(x: f64, z: f64) -> Keyframe {
        Keyframe {
            position: [x, 10.0, z],
            target: [x, 0.0, z - 100.0],
        }
    }

    fn path() -> CameraPath {
        CameraPath::new(vec![key(0.0, 0.0), key(100.0, 0.0), key(100.0, 200.0)])
    }

    #[test]
    fn the_first_and_last_frames_are_the_first_and_last_keyframes() {
        let path = path();
        assert_eq!(
            path.camera_at(0, 101),
            Camera::looking_at([0.0, 10.0, 0.0], [0.0, 0.0, -100.0])
        );
        assert_eq!(
            path.camera_at(100, 101),
            Camera::looking_at([100.0, 10.0, 200.0], [100.0, 0.0, 100.0])
        );
    }

    #[test]
    fn frames_in_between_are_on_the_straight_line_between_keyframes() {
        // 101 frames over 2 segments: the middle keyframe is at frame 50, so frame 25 is halfway
        // along the first segment.
        let path = path();
        let halfway = path.camera_at(25, 101);
        assert_eq!(halfway.position, [50.0, 10.0, 0.0]);
        assert_eq!(path.camera_at(50, 101).position, [100.0, 10.0, 0.0]);
        assert_eq!(path.camera_at(75, 101).position, [100.0, 10.0, 100.0]);
        // The look target is interpolated too, not only the position.
        assert_eq!(
            halfway,
            Camera::looking_at([50.0, 10.0, 0.0], [50.0, 0.0, -100.0])
        );
    }

    #[test]
    fn a_radial_path_looks_at_its_target_with_the_horizon_level_to_the_planets_up() {
        // Far from the +y pole, where looking_at's up is not the Planet's.
        let position = [90_000.0, -60_000.0, 120_000.0];
        let target = [90_050.0, -60_080.0, 119_990.0];
        let path = CameraPath::new(vec![
            Keyframe { position, target },
            Keyframe { position, target },
        ])
        .radial();
        let camera = path.camera_at(0, 10);
        let d: [f64; 3] = std::array::from_fn(|a| target[a] - position[a]);
        let length = d.iter().map(|c| c * c).sum::<f64>().sqrt();
        for (got, want) in camera.forward().into_iter().zip(d) {
            assert!((got - want / length).abs() < 1e-9, "{got} {want}");
        }
        let radius = position.iter().map(|c| c * c).sum::<f64>().sqrt();
        for (got, want) in camera.horizon_up.into_iter().zip(position) {
            assert!((got - want / radius).abs() < 1e-12);
        }
    }

    #[test]
    fn the_camera_depends_only_on_the_frame_index() {
        let path = path();
        let again = path.clone();
        for frame in [0, 7, 33, 100] {
            assert_eq!(path.camera_at(frame, 101), again.camera_at(frame, 101));
        }
    }

    #[test]
    fn a_path_without_rolls_keeps_the_horizon_level() {
        assert_eq!(path().camera_at(30, 101).roll, 0.0);
    }

    #[test]
    fn rolls_are_blended_between_keyframes_and_exact_on_them() {
        let path = path().rolled(vec![0.0, 0.4, -0.2]);
        assert_eq!(path.camera_at(0, 101).roll, 0.0);
        assert!((path.camera_at(25, 101).roll - 0.2).abs() < 1e-12);
        assert_eq!(path.camera_at(50, 101).roll, 0.4);
        assert!((path.camera_at(75, 101).roll - 0.1).abs() < 1e-12);
        assert_eq!(path.camera_at(100, 101).roll, -0.2);
        // Roll turns the view about where it looks and moves nothing.
        let level = self::path().camera_at(25, 101);
        let rolled = path.camera_at(25, 101);
        assert_eq!(
            (rolled.position, rolled.forward()),
            (level.position, level.forward())
        );
        assert_ne!(rolled.right(), level.right());
    }

    #[test]
    #[should_panic(expected = "one roll for each keyframe")]
    fn a_path_needs_a_roll_for_every_keyframe() {
        let _ = path().rolled(vec![0.0, 0.1]);
    }

    #[test]
    fn a_frame_past_the_end_stays_on_the_last_keyframe() {
        assert_eq!(path().camera_at(500, 101), path().camera_at(100, 101));
    }

    #[test]
    fn a_run_of_one_frame_is_the_first_keyframe() {
        assert_eq!(path().camera_at(0, 1), path().camera_at(0, 101));
    }

    #[test]
    fn screenshots_are_taken_where_the_camera_is_on_a_keyframe() {
        assert_eq!(path().keyframe_frames(101), [0, 50, 100]);
        // Frames that do not divide evenly round to the nearest frame.
        assert_eq!(path().keyframe_frames(10), [0, 5, 9]);
        // Too short a run to tell keyframes apart: no duplicates.
        assert_eq!(path().keyframe_frames(2), [0, 1]);
        assert_eq!(path().keyframe_frames(1), [0]);
    }
}
