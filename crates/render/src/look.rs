//! The light's palette: the colours of the sun and of the sky, and the haze (#202, the owner's
//! picks of #148). Both views read the same one, so they differ only by what ADR 0005 allows.
//!
//! The default is **hero light B** ("golden"), picked from the look-dev contact sheet: the sun
//! low and red, a dark blue zenith fading to an orange horizon, and warm haze. [`Look::LEGACY`]
//! is what the renderer had before (a neutral high sun and a pale blue sky, no haze): the tests
//! whose answer is worked out from the colours, and the bench's convergence phase, whose numbers
//! are compared across reports, use it.
//!
//! The sky is the atmosphere's ([`Look::atmosphere`], #53) when the look has one: the sun's light
//! is what the air lets through, and the sky and the haze are what it scatters. Without one
//! (`atmosphere: None`: the legacy look, and hero light B's gradient, which the look-dev sheet
//! shows beside the atmosphere) the sky is a gradient along the local up, which is the camera's
//! `horizon_up` (the Planet's radial up once the camera is carried over a Planet, the world's +y
//! otherwise), with the haze that fades distant surfaces to its horizon.

use bytemuck::{Pod, Zeroable};

use crate::atmosphere::Atmosphere;

/// The sun's and the sky's colours, and how thick the air is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// The sun's light on a surface that faces it head on, as a multiple of the surface's colour
    /// (its irradiance divided by pi); linear RGB. May be above 1.
    pub sun: [f32; 3],
    /// The sky straight up, linear RGB.
    pub zenith: [f32; 3],
    /// The sky at the horizon and below it, linear RGB: also the colour that haze fades to.
    pub horizon: [f32; 3],
    /// Haze per metre, along the camera ray: a surface `d` metres away is seen as
    /// `exp(-haze * d)` of its own light and the rest of the horizon's colour. 0 for none.
    pub haze_per_metre: f32,
    /// The Planet's air, which makes the sun's light, the sky and the haze when there is one; the
    /// fields above are then the gradient it replaces, and are not used.
    pub atmosphere: Option<Atmosphere>,
}

impl Look {
    /// Hero light B, "golden": the sun 8 degrees above the horizon (the elevation is the sun's,
    /// [`crate::sun::DEFAULT_ELEVATION`]), a redder sun, a darker zenith, an orange horizon and
    /// haze of 1/300 per metre. The values are the owner's pick of #148 (the prototype's
    /// `HERO_B`).
    pub const HERO_B: Look = Look {
        sun: [1.3, 0.80, 0.40],
        zenith: [0.12, 0.18, 0.45],
        horizon: [1.0, 0.55, 0.38],
        haze_per_metre: 1.0 / 300.0,
        atmosphere: Some(Atmosphere::HERO),
    };

    /// Hero light B as it was before the atmosphere: the owner's gradient and haze, which the
    /// look-dev sheet shows beside it.
    pub const HERO_B_GRADIENT: Look = Look {
        atmosphere: None,
        ..Self::HERO_B
    };

    /// What the renderer had before #202: a neutral sun, a pale sky and no haze.
    pub const LEGACY: Look = Look {
        sun: [1.6, 1.5, 1.35],
        zenith: [0.25, 0.45, 0.85],
        horizon: [0.75, 0.82, 0.92],
        haze_per_metre: 0.0,
        atmosphere: None,
    };

    /// Haze per Voxel, which is what the shaders work in (distances along a ray are in Voxels).
    pub fn haze_per_voxel(&self) -> f32 {
        (f64::from(self.haze_per_metre) / brick_store::VOXELS_PER_METRE) as f32
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::HERO_B
    }
}

/// The look as the shaders read it (`look_*` and `haze` fields of `Frame` in `common.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct LookUniform {
    /// The sun's radiance, then one unused.
    pub sun: [f32; 4],
    /// The sky at the zenith, then one unused.
    pub zenith: [f32; 4],
    /// The sky at the horizon, then the haze per Voxel.
    pub horizon_and_haze: [f32; 4],
}

impl Look {
    pub fn uniform(&self) -> LookUniform {
        let [sr, sg, sb] = self.sun;
        let [zr, zg, zb] = self.zenith;
        let [hr, hg, hb] = self.horizon;
        LookUniform {
            sun: [sr, sg, sb, 0.0],
            zenith: [zr, zg, zb, 0.0],
            horizon_and_haze: [hr, hg, hb, self.haze_per_voxel()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_hero_light_b_with_the_owners_values() {
        let look = Look::default();
        assert_eq!(look.sun, [1.3, 0.80, 0.40]);
        assert_eq!(look.zenith, [0.12, 0.18, 0.45]);
        assert_eq!(look.horizon, [1.0, 0.55, 0.38]);
        assert_eq!(look.haze_per_metre, 1.0 / 300.0);
    }

    #[test]
    fn haze_is_told_to_the_shaders_per_voxel() {
        // 1/300 per metre at 20 Voxels to the metre.
        let per_voxel = Look::HERO_B.uniform().horizon_and_haze[3];
        assert!((per_voxel - 1.0 / 6000.0).abs() < 1e-9, "{per_voxel}");
        assert_eq!(Look::LEGACY.uniform().horizon_and_haze[3], 0.0);
    }

    #[test]
    fn the_legacy_look_is_the_colours_the_shaders_had_hard_coded() {
        let u = Look::LEGACY.uniform();
        assert_eq!(u.sun[..3], [1.6, 1.5, 1.35]);
        assert_eq!(u.zenith[..3], [0.25, 0.45, 0.85]);
        assert_eq!(u.horizon_and_haze[..3], [0.75, 0.82, 0.92]);
    }
}
