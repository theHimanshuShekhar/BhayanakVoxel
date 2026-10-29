//! The free-fly camera and what is uploaded of it.
//!
//! The camera lives in f64 on the CPU, in Voxel units. The GPU gets it camera-relative: the
//! integer Voxel the camera is in, and its position inside that Voxel. Every float the shader
//! computes is a distance from the camera, so precision depends on how far away something is
//! and never on how far the camera is from the Planet's origin.

use bytemuck::{Pod, Zeroable};

/// Splits a position into the Voxel containing it and the offset inside that Voxel, in `0..1`.
/// The subtraction is exact in f64 for any position an `i32` Voxel coordinate can name.
pub fn split(position: [f64; 3]) -> ([i32; 3], [f32; 3]) {
    let cell = position.map(|p| p.floor());
    let offset = [0, 1, 2].map(|a| (position[a] - cell[a]) as f32);
    (cell.map(|c| c as i32), offset)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Position in Voxels, Planet frame.
    pub position: [f64; 3],
    /// Radians anticlockwise seen from above (`horizon_up`). Zero looks along `-back`.
    pub yaw: f64,
    /// Radians above the horizon, kept within `MAX_PITCH`.
    pub pitch: f64,
    /// Unit vector the horizon is level to, which yaw turns around and pitch is measured from:
    /// +y until `carry_up` moves it, then the Planet's radial up. Not the view's `up()`.
    pub horizon_up: [f64; 3],
    /// Unit vector level with the horizon, opposite to where zero yaw looks: +z until `carry_up`
    /// moves it.
    ///
    /// `horizon_up` and `back` must stay orthonormal, so change them only through `carry_up`,
    /// and call it with the new position whenever `position` moves over a Planet.
    pub back: [f64; 3],
    /// Vertical field of view in radians.
    pub fov_y: f64,
}

/// Just short of straight up or down, where "right" would be undefined.
const MAX_PITCH: f64 = 89.0 * std::f64::consts::PI / 180.0;

/// Closer to the Planet's centre than this, "up" is undefined and `carry_up` keeps the old one.
const MIN_UP_LENGTH: f64 = 1e-6;

/// The camera as the shader reads it (`Camera` in `common.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct CameraUniform {
    /// The Voxel the camera is in; the fourth lane is unused.
    pub cell: [i32; 4],
    /// The camera's position inside that Voxel.
    pub origin: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    pub forward: [f32; 4],
    /// Tangent of half the vertical field of view; the rest is unused.
    pub params: [f32; 4],
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

impl Camera {
    /// A camera at `position` looking at `target`, with a 70 degree vertical field of view.
    pub fn looking_at(position: [f64; 3], target: [f64; 3]) -> Self {
        let d = [0, 1, 2].map(|a| target[a] - position[a]);
        let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
        Camera {
            position,
            yaw: (-d[0]).atan2(-d[2]),
            pitch: (d[1] / len).asin(),
            horizon_up: [0.0, 1.0, 0.0],
            back: [0.0, 0.0, 1.0],
            fov_y: 70f64.to_radians(),
        }
    }

    /// Unit vector the camera looks along.
    pub fn forward(&self) -> [f64; 3] {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let side = cross(self.horizon_up, self.back);
        [0, 1, 2].map(|a| {
            -sin_yaw * cos_pitch * side[a]
                + sin_pitch * self.horizon_up[a]
                + -cos_yaw * cos_pitch * self.back[a]
        })
    }

    /// Unit vector to the camera's right, level with the horizon.
    pub fn right(&self) -> [f64; 3] {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let side = cross(self.horizon_up, self.back);
        [0, 1, 2].map(|a| cos_yaw * side[a] + -sin_yaw * self.back[a])
    }

    /// Unit vector up from the camera's view.
    pub fn up(&self) -> [f64; 3] {
        cross(self.right(), self.forward())
    }

    /// Turns the camera: `yaw` anticlockwise seen from above, `pitch` upwards, in radians.
    pub fn turn(&mut self, yaw: f64, pitch: f64) {
        self.yaw = (self.yaw + yaw).rem_euclid(std::f64::consts::TAU);
        self.pitch = (self.pitch + pitch).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Levels the horizon to `up` (for a Planet at the origin, the camera's position), keeping
    /// the heading by the smallest rotation: parallel transport, which has no singularity
    /// anywhere on a sphere. Does nothing if `up` is (nearly) zero.
    pub fn carry_up(&mut self, up: [f64; 3]) {
        let len = dot(up, up).sqrt();
        if len < MIN_UP_LENGTH {
            return;
        }
        let up = up.map(|c| c / len);
        let cos = dot(self.horizon_up, up);
        // Rodrigues' rotation taking the old up to the new one, applied to the heading. Exactly
        // opposite ups have no smallest rotation; the levelling below is all they get.
        if cos > -1.0 + 1e-9 {
            let axis = cross(self.horizon_up, up);
            let swung = cross(axis, self.back);
            let twice = cross(axis, swung);
            self.back = [0, 1, 2].map(|a| self.back[a] + swung[a] + twice[a] / (1.0 + cos));
        }
        // Drops what rounding leaves along the new up.
        let tilt = dot(self.back, up);
        let back = [0, 1, 2].map(|a| self.back[a] - up[a] * tilt);
        let len = dot(back, back).sqrt();
        self.horizon_up = up;
        self.back = back.map(|c| c / len);
    }

    pub fn uniform(&self) -> CameraUniform {
        let (cell, origin) = split(self.position);
        let lanes = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32, 0.0];
        CameraUniform {
            cell: [cell[0], cell[1], cell[2], 0],
            origin: [origin[0], origin[1], origin[2], 0.0],
            right: lanes(self.right()),
            up: lanes(self.up()),
            forward: lanes(self.forward()),
            params: [(self.fov_y / 2.0).tan() as f32, 0.0, 0.0, 0.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-12)
    }

    #[test]
    fn the_default_view_looks_down_negative_z_with_y_up() {
        let camera = Camera::looking_at([0.0, 0.0, 5.0], [0.0, 0.0, 0.0]);
        assert!(near(camera.forward(), [0.0, 0.0, -1.0]));
        assert!(near(camera.right(), [1.0, 0.0, 0.0]));
        assert!(near(camera.up(), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn looking_at_a_target_points_the_camera_at_it() {
        let from = [100.0, -40.0, 7.0];
        let target = [-20.0, 60.0, 300.0];
        let d = [0, 1, 2].map(|a| target[a] - from[a]);
        let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
        let camera = Camera::looking_at(from, target);
        assert!(near(camera.forward(), d.map(|c| c / len)));
    }

    #[test]
    fn turning_left_swings_the_view_towards_negative_x_and_pitch_is_clamped() {
        let mut camera = Camera::looking_at([0.0; 3], [0.0, 0.0, -1.0]);
        camera.turn(std::f64::consts::FRAC_PI_2, 0.0);
        assert!(near(camera.forward(), [-1.0, 0.0, 0.0]));
        camera.turn(0.0, 10.0);
        assert!(camera.forward()[1] < 1.0 && camera.forward()[1] > 0.99);
        camera.turn(0.0, -100.0);
        assert!(camera.forward()[1] > -1.0 && camera.forward()[1] < -0.99);
    }

    #[test]
    fn the_basis_stays_orthonormal() {
        let mut camera = Camera::looking_at([1.0; 3], [5.0, 3.0, -9.0]);
        camera.turn(2.3, -0.4);
        let (f, r, u) = (camera.forward(), camera.right(), camera.up());
        let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f64>();
        for (a, b) in [(f, r), (f, u), (r, u)] {
            assert!(dot(a, b).abs() < 1e-12);
        }
        assert!((dot(u, u) - 1.0).abs() < 1e-12);
    }

    /// Where the Planet's radial up is the +x, -x, +y, -y, +z, -z axes, and off them.
    const SPOTS: [[f64; 3]; 8] = [
        [3000.0, 0.0, 0.0],
        [-3000.0, 0.0, 0.0],
        [0.0, 3000.0, 0.0],
        [0.0, -3000.0, 0.0],
        [0.0, 0.0, 3000.0],
        [0.0, 0.0, -3000.0],
        [1000.0, 2000.0, -1500.0],
        [-0.5, 3000.0, 0.5],
    ];

    fn carried_to(position: [f64; 3]) -> Camera {
        let mut camera = Camera::looking_at([0.0, 0.0, 5.0], [1.0, 0.5, -3.0]);
        camera.turn(1.0, 0.3);
        camera.position = position;
        camera.carry_up(position);
        camera
    }

    #[test]
    fn right_stays_level_with_the_radial_up_all_over_the_sphere() {
        for spot in SPOTS {
            let camera = carried_to(spot);
            let radial = spot.map(|c| c / dot(spot, spot).sqrt());
            assert!(dot(camera.right(), radial).abs() < 1e-12, "at {spot:?}");
            assert!(dot(camera.up(), radial) > 0.0, "at {spot:?}");
            let (f, r, u) = (camera.forward(), camera.right(), camera.up());
            for (a, b) in [(f, r), (f, u), (r, u)] {
                assert!(dot(a, b).abs() < 1e-12, "at {spot:?}");
            }
        }
    }

    #[test]
    fn pitch_is_measured_from_the_local_horizon_and_clamped() {
        for spot in SPOTS {
            let mut camera = carried_to(spot);
            let radial = spot.map(|c| c / dot(spot, spot).sqrt());
            camera.turn(0.0, 0.2 - camera.pitch);
            assert!((dot(camera.forward(), radial) - 0.2f64.sin()).abs() < 1e-12);
            camera.turn(0.0, 10.0);
            let above = dot(camera.forward(), radial);
            assert!(above < 1.0 && above > 0.99, "at {spot:?}");
            camera.turn(0.0, -100.0);
            let below = dot(camera.forward(), radial);
            assert!(below > -1.0 && below < -0.99, "at {spot:?}");
        }
    }

    #[test]
    fn yaw_turns_the_heading_around_the_local_up() {
        for spot in SPOTS {
            let mut camera = carried_to(spot);
            let radial = spot.map(|c| c / dot(spot, spot).sqrt());
            camera.turn(0.0, -camera.pitch);
            let before = camera.forward();
            camera.turn(0.5, 0.0);
            let after = camera.forward();
            assert!(dot(after, radial).abs() < 1e-12, "at {spot:?}");
            // Anticlockwise seen from above: before rotated about the up by 0.5 radians.
            let side = cross(radial, before);
            let expected = [0, 1, 2].map(|a| before[a] * 0.5f64.cos() + side[a] * 0.5f64.sin());
            assert!(near(after, expected), "at {spot:?}");
        }
    }

    #[test]
    fn carrying_up_to_the_centre_changes_nothing() {
        let mut camera = carried_to([1000.0, 2000.0, -1500.0]);
        let before = camera;
        camera.carry_up([0.0; 3]);
        camera.carry_up([1e-9, 0.0, 0.0]);
        assert_eq!(camera, before);
    }

    #[test]
    fn carrying_up_to_the_opposite_direction_stays_finite_and_level() {
        let mut camera = Camera::looking_at([0.0, 5.0, 0.0], [3.0, 0.0, 1.0]);
        camera.carry_up([0.0, -5.0, 0.0]);
        assert!(camera.forward().iter().all(|c| c.is_finite()));
        assert!(dot(camera.right(), [0.0, -1.0, 0.0]).abs() < 1e-12);
    }

    #[test]
    fn the_split_is_exact_for_negative_and_fractional_positions() {
        assert_eq!(split([1.25, -0.75, 0.0]), ([1, -1, 0], [0.25, 0.25, 0.0]));
    }

    #[test]
    fn far_from_the_origin_the_upload_is_still_small_and_exact() {
        // A single f32 position loses everything below 1 Voxel out here: 2^27 + 0.375 rounds to
        // 2^27. Split into a Voxel and an offset, the offset keeps the fraction.
        let far = f64::from(1u32 << 27);
        let mut camera = Camera::looking_at([far + 0.375, -far - 0.625, 5.5], [0.0; 3]);
        camera.fov_y = 1.0;
        let u = camera.uniform();
        assert_eq!(u.cell[..3], [1 << 27, -(1 << 27) - 1, 5]);
        assert_eq!(u.origin, [0.375, 0.375, 0.5, 0.0]);
        assert_eq!(
            (far + 0.375) as f32 as f64,
            far,
            "a plain f32 would lose the fraction"
        );
        // Moving by an eighth of a Voxel moves the upload by an eighth.
        camera.position[0] += 0.125;
        assert_eq!(camera.uniform().origin[0], 0.5);
    }
}
