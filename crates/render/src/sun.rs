//! The sun: one directional light with a small angular size, so that shadows have soft edges.

use bytemuck::{Pod, Zeroable};

/// A little over 2 degrees, several times the real sun's 0.27, so that penumbrae are wide enough
/// to see at Voxel scale.
pub const DEFAULT_ANGULAR_RADIUS: f32 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    /// Unit vector from the surface towards the sun.
    direction: [f32; 3],
    /// Half the angle the sun's disc spans, in radians.
    angular_radius: f32,
}

/// The sun as the shaders read it (the `sun` field of `Frame` in `common.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct SunUniform {
    /// The direction towards the sun, then the cosine of its angular radius.
    pub direction_and_cos_radius: [f32; 4],
}

impl Sun {
    /// A sun towards `direction` (any length), or `None` if that is zero or not finite.
    /// `angular_radius` is clamped to 0 (a point: hard shadows) up to a quarter turn.
    pub fn new(direction: [f32; 3], angular_radius: f32) -> Option<Self> {
        let length = direction.iter().map(|c| c * c).sum::<f32>().sqrt();
        if !length.is_finite() || length == 0.0 || angular_radius.is_nan() {
            return None;
        }
        Some(Self {
            direction: direction.map(|c| c / length),
            angular_radius: angular_radius.clamp(0.0, std::f32::consts::FRAC_PI_2),
        })
    }

    /// A sun `elevation` radians above the horizon (the xz plane) at `azimuth` radians, measured
    /// from +z towards +x.
    pub fn from_angles(azimuth: f32, elevation: f32, angular_radius: f32) -> Self {
        let (sin_az, cos_az) = azimuth.sin_cos();
        let (sin_el, cos_el) = elevation.sin_cos();
        Self::new([sin_az * cos_el, sin_el, cos_az * cos_el], angular_radius)
            .expect("finite angles give a direction")
    }

    /// The azimuth and elevation that `from_angles` takes.
    pub fn angles(&self) -> (f32, f32) {
        let [x, y, z] = self.direction;
        (x.atan2(z), y.asin())
    }

    pub fn direction(&self) -> [f32; 3] {
        self.direction
    }

    pub fn angular_radius(&self) -> f32 {
        self.angular_radius
    }

    pub fn uniform(&self) -> SunUniform {
        let [x, y, z] = self.direction;
        SunUniform {
            direction_and_cos_radius: [x, y, z, self.angular_radius.cos()],
        }
    }
}

impl Default for Sun {
    /// The direction the primary-visibility shading has always used, high in the sky.
    fn default() -> Self {
        Self::new([0.5, 0.8, 0.3], DEFAULT_ANGULAR_RADIUS).expect("a valid direction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn length(v: [f32; 3]) -> f32 {
        v.iter().map(|c| c * c).sum::<f32>().sqrt()
    }

    #[test]
    fn the_direction_is_normalised_whatever_length_it_is_given() {
        for d in [[0.0, 7.0, 0.0], [3.0, 4.0, 12.0], [-1e-3, 2e-3, 0.5e-3]] {
            let sun = Sun::new(d, 0.01).unwrap();
            assert!((length(sun.direction()) - 1.0).abs() < 1e-6, "{d:?}");
        }
        assert_eq!(
            Sun::new([0.0, 5.0, 0.0], 0.0).unwrap().direction(),
            [0.0, 1.0, 0.0]
        );
        assert!((length(Sun::default().direction()) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_direction_with_no_heading_is_rejected() {
        assert_eq!(Sun::new([0.0; 3], 0.01), None);
        assert_eq!(Sun::new([f32::NAN, 1.0, 0.0], 0.01), None);
        assert_eq!(Sun::new([f32::INFINITY, 1.0, 0.0], 0.01), None);
        assert_eq!(Sun::new([0.0, 1.0, 0.0], f32::NAN), None);
    }

    #[test]
    fn the_angular_radius_is_kept_within_zero_and_a_quarter_turn() {
        let radius = |r| Sun::new([0.0, 1.0, 0.0], r).unwrap().angular_radius();
        assert_eq!(radius(0.02), 0.02);
        assert_eq!(radius(-1.0), 0.0);
        assert_eq!(radius(10.0), std::f32::consts::FRAC_PI_2);
    }

    #[test]
    fn angles_give_the_expected_unit_directions() {
        let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6);
        let at = |az, el| Sun::from_angles(az, el, 0.0).direction();
        assert!(near(at(0.0, 0.0), [0.0, 0.0, 1.0]));
        assert!(near(at(std::f32::consts::FRAC_PI_2, 0.0), [1.0, 0.0, 0.0]));
        assert!(near(at(1.0, std::f32::consts::FRAC_PI_2), [0.0, 1.0, 0.0]));
        assert!((length(at(2.0, 0.7)) - 1.0).abs() < 1e-6);
        assert!(at(2.0, 0.7)[1] > 0.0);
    }

    #[test]
    fn angles_are_the_inverse_of_from_angles() {
        for (azimuth, elevation) in [(0.0, 0.0), (1.0, 0.5), (-2.5, 1.2), (3.0, -0.3)] {
            let (a, e) = Sun::from_angles(azimuth, elevation, 0.0).angles();
            assert!((a - azimuth).abs() < 1e-5 && (e - elevation).abs() < 1e-5);
        }
        // The default sun can be turned from where it is.
        let (a, e) = Sun::default().angles();
        let again = Sun::from_angles(a, e, DEFAULT_ANGULAR_RADIUS);
        assert!(
            (0..3).all(|i| (again.direction()[i] - Sun::default().direction()[i]).abs() < 1e-6)
        );
    }

    #[test]
    fn the_uniform_carries_the_direction_and_the_cosine_of_the_radius() {
        let sun = Sun::new([0.0, 0.0, 2.0], 0.5).unwrap();
        let [x, y, z, w] = sun.uniform().direction_and_cos_radius;
        assert_eq!([x, y, z], [0.0, 0.0, 1.0]);
        assert!((w - 0.5f32.cos()).abs() < 1e-6);
    }
}
