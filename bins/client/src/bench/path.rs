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
}

impl CameraPath {
    /// A path through `keyframes`, which needs at least two.
    pub fn new(keyframes: Vec<Keyframe>) -> Self {
        assert!(keyframes.len() >= 2, "a path needs at least two keyframes");
        Self { keyframes }
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
        Camera::looking_at(
            lerp(from.position, to.position),
            lerp(from.target, to.target),
        )
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
    fn the_camera_depends_only_on_the_frame_index() {
        let path = path();
        let again = path.clone();
        for frame in [0, 7, 33, 100] {
            assert_eq!(path.camera_at(frame, 101), again.camera_at(frame, 101));
        }
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
