//! PROTOTYPE (#148): the look-dev post-processing, on the CPU, on the linear HDR image that the
//! reference path tracer accumulates. Tone curves, exposure (fixed and adapting) and bloom: none
//! of which the renderer has yet (#52-#55), so this is where the dev gets to see them.

/// A linear HDR image: RGB, rows from the top.
#[derive(Clone)]
pub struct Hdr {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<[f32; 3]>,
}

/// The tone curves of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// What the renderer does today: clamp, then the 1/2.2 gamma.
    None,
    /// Narkowicz's fit of the ACES filmic curve.
    Aces,
    /// An AgX-like curve (Sobotka's, in the polynomial form of Wrensch).
    AgX,
    /// The Khronos PBR Neutral tone mapper: colour-faithful until it is close to white.
    Neutral,
}

impl Tone {
    pub const ALL: [Tone; 4] = [Tone::None, Tone::Aces, Tone::AgX, Tone::Neutral];

    pub fn name(self) -> &'static str {
        match self {
            Tone::None => "none (today)",
            Tone::Aces => "ACES (Narkowicz fit)",
            Tone::AgX => "AgX (approximation)",
            Tone::Neutral => "Khronos PBR Neutral",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Tone::None => "none",
            Tone::Aces => "aces",
            Tone::AgX => "agx",
            Tone::Neutral => "neutral",
        }
    }

    /// The display value (0..1, already encoded) of a linear, exposed colour.
    pub fn apply(self, c: [f32; 3]) -> [f32; 3] {
        let encode = |v: f32| v.clamp(0.0, 1.0).powf(1.0 / 2.2);
        match self {
            Tone::None => c.map(encode),
            Tone::Aces => c.map(|x| {
                let x = x.max(0.0);
                encode((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14))
            }),
            Tone::AgX => agx(c),
            Tone::Neutral => neutral(c).map(encode),
        }
    }
}

fn mul(m: [[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

fn agx(c: [f32; 3]) -> [f32; 3] {
    const IN: [[f32; 3]; 3] = [
        [0.842_479, 0.078_433_6, 0.079_223_74],
        [0.042_328_24, 0.878_468_6, 0.079_166_13],
        [0.042_375_65, 0.078_433_6, 0.879_142_97],
    ];
    const OUT: [[f32; 3]; 3] = [
        [1.196_879, -0.098_020_88, -0.099_029_74],
        [-0.052_896_85, 1.151_903_1, -0.098_961_18],
        [-0.052_971_64, -0.098_043_45, 1.151_073_7],
    ];
    const MIN_EV: f32 = -12.473_93;
    const MAX_EV: f32 = 4.026_069;
    let v = mul(IN, c.map(|x| x.max(1e-10)));
    let v = v.map(|x| ((x.log2().clamp(MIN_EV, MAX_EV)) - MIN_EV) / (MAX_EV - MIN_EV));
    let contrast = |x: f32| {
        let (x2, x4) = (x * x, x * x * x * x);
        15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x
            - 0.00232
    };
    mul(OUT, v.map(contrast)).map(|x| x.clamp(0.0, 1.0))
}

fn neutral(c: [f32; 3]) -> [f32; 3] {
    const START: f32 = 0.8 - 0.04;
    const DESATURATION: f32 = 0.15;
    let low = c[0].min(c[1]).min(c[2]);
    let offset = if low < 0.08 {
        low - 6.25 * low * low
    } else {
        0.04
    };
    let c = c.map(|x| x - offset);
    let peak = c[0].max(c[1]).max(c[2]);
    if peak < START {
        return c;
    }
    let d = 1.0 - START;
    let new_peak = 1.0 - d * d / (peak + d - START);
    let c = c.map(|x| x * new_peak / peak);
    let g = 1.0 - 1.0 / (DESATURATION * (peak - new_peak) + 1.0);
    c.map(|x| x + (new_peak - x) * g)
}

impl Hdr {
    /// The mean of the log of the luminance: what auto-exposure aims at.
    pub fn log_luminance(&self) -> f32 {
        let sum: f32 = self
            .rgb
            .iter()
            .map(|c| {
                (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
                    .max(1e-4)
                    .ln()
            })
            .sum();
        sum / self.rgb.len() as f32
    }

    /// The image under `tone` after it is scaled by `exposure` and has `bloom` (0 for none)
    /// added, as RGBA8. Bloom is on the exposed image, as a camera would see it.
    pub fn develop(&self, tone: Tone, exposure: f32, bloom: f32) -> render::Image {
        let exposed = Hdr {
            rgb: self.rgb.iter().map(|c| c.map(|x| x * exposure)).collect(),
            ..self.clone()
        };
        let exposed = if bloom > 0.0 {
            exposed.bloom(bloom)
        } else {
            exposed
        };
        let mut rgba = Vec::with_capacity(self.rgb.len() * 4);
        for c in &exposed.rgb {
            let d = tone.apply(*c);
            rgba.extend(d.map(|x| (x * 255.0 + 0.5) as u8));
            rgba.push(255);
        }
        render::Image {
            width: self.width,
            height: self.height,
            rgba,
        }
    }

    /// The image with a glow added: the image blurred at three scales, each `strength` of it.
    /// Linear, so it commutes with exposure.
    pub fn bloom(&self, strength: f32) -> Hdr {
        let scale = self.width as f32 / 640.0;
        let blurred: Vec<Vec<[f32; 3]>> = [2.5, 7.0, 18.0]
            .iter()
            .map(|s| {
                blur(
                    &self.rgb,
                    self.width as usize,
                    self.height as usize,
                    s * scale,
                )
            })
            .collect();
        let rgb = (0..self.rgb.len())
            .map(|i| {
                std::array::from_fn(|k| {
                    let glow: f32 = blurred.iter().map(|b| b[i][k]).sum::<f32>() / 3.0;
                    self.rgb[i][k] + strength * glow
                })
            })
            .collect();
        Hdr {
            rgb,
            ..self.clone()
        }
    }
}

/// A separable Gaussian blur of `sigma` pixels.
fn blur(src: &[[f32; 3]], w: usize, h: usize, sigma: f32) -> Vec<[f32; 3]> {
    let radius = (sigma * 3.0).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let pass = |from: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 3];
                for (k, weight) in kernel.iter().enumerate() {
                    let d = k as isize - radius;
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    let c = from[sy * w + sx];
                    for a in 0..3 {
                        acc[a] += c[a] * weight;
                    }
                }
                out[y * w + x] = acc.map(|v| v / total);
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

/// How the exposure follows the scene's brightness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exposure {
    /// One value for every frame: what the renderer does today (1).
    Fixed(f32),
    /// Adapts to the mean log luminance with this time constant, in seconds.
    Adapting { tau: f32 },
}

/// The brightest and darkest the auto-exposure goes: -3 to +5 stops.
pub const EXPOSURE_RANGE: (f32, f32) = (0.125, 32.0);
/// The grey that auto-exposure aims the scene's mean at.
pub const MIDDLE_GREY: f32 = 0.18;

/// The exposure that puts a scene of mean log luminance `log_luminance` at middle grey.
pub fn settled_exposure(log_luminance: f32) -> f32 {
    (MIDDLE_GREY / log_luminance.exp()).clamp(EXPOSURE_RANGE.0, EXPOSURE_RANGE.1)
}

/// The exposure at each of a run of frames `dt` seconds apart, for scenes whose settled exposures
/// are `targets`, as `exposure` makes it. The eye is taken to start settled.
pub fn exposure_curve(exposure: Exposure, targets: &[f32], dt: f32) -> Vec<f32> {
    match exposure {
        Exposure::Fixed(e) => vec![e; targets.len()],
        Exposure::Adapting { tau } => {
            // Adapts in stops, which is how the eye does it: a step of N stops takes the same
            // time to cross, bright to dark or dark to bright.
            let mut stops = targets[0].log2();
            let k = 1.0 - (-dt / tau).exp();
            targets
                .iter()
                .map(|t| {
                    stops += (t.log2() - stops) * k;
                    stops.exp2()
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(c: [f32; 3]) -> Hdr {
        Hdr {
            width: 2,
            height: 2,
            rgb: vec![c; 4],
        }
    }

    #[test]
    fn no_curve_is_the_renderers_clamp_and_gamma() {
        let [r, g, b] = Tone::None.apply([0.5, 2.0, 0.0]);
        assert!((r - 0.5f32.powf(1.0 / 2.2)).abs() < 1e-6);
        assert_eq!((g, b), (1.0, 0.0));
    }

    #[test]
    fn the_filmic_curves_keep_a_bright_light_below_white_where_clamping_does_not() {
        let hot = [4.0, 1.0, 0.2];
        assert_eq!(Tone::None.apply(hot)[0], 1.0);
        for tone in [Tone::Aces, Tone::AgX, Tone::Neutral] {
            let d = tone.apply(hot);
            assert!(d.iter().all(|c| (0.0..=1.0).contains(c)), "{tone:?} {d:?}");
            assert!(d[0] >= d[1] && d[1] >= d[2], "{tone:?} keeps the hue order");
        }
    }

    #[test]
    fn the_curves_rise_with_the_light() {
        for tone in Tone::ALL {
            let mut last = -1.0;
            for stop in -6..4 {
                let v = tone.apply([2f32.powi(stop); 3])[1];
                assert!(v >= last, "{tone:?} at {stop}");
                last = v;
            }
        }
    }

    #[test]
    fn settled_exposure_puts_the_mean_at_middle_grey_within_its_range() {
        assert!((settled_exposure(0.5f32.ln()) - 0.36).abs() < 1e-5);
        assert_eq!(settled_exposure(1e-6f32.ln()), EXPOSURE_RANGE.1);
        assert_eq!(settled_exposure(100f32.ln()), EXPOSURE_RANGE.0);
    }

    #[test]
    fn adapting_exposure_trails_the_scene_and_the_faster_one_catches_up_first() {
        // Bright for 2 frames, then dark, a second apart.
        let targets = [1.0, 1.0, 16.0, 16.0, 16.0, 16.0];
        let fast = exposure_curve(Exposure::Adapting { tau: 0.5 }, &targets, 1.0);
        let slow = exposure_curve(Exposure::Adapting { tau: 3.0 }, &targets, 1.0);
        let fixed = exposure_curve(Exposure::Fixed(1.0), &targets, 1.0);
        assert_eq!(fixed, [1.0; 6]);
        assert_eq!(fast[1], 1.0);
        assert!(fast[3] > slow[3], "the fast eye is further along");
        assert!(slow[5] > slow[3] && slow[5] < 16.0);
    }

    #[test]
    fn bloom_adds_light_around_a_bright_pixel_and_changes_nothing_in_a_flat_image() {
        let flat_image = flat([0.5; 3]).bloom(0.1);
        assert!(flat_image.rgb.iter().all(|c| (c[0] - 0.55).abs() < 1e-4));
        let mut dot = Hdr {
            width: 32,
            height: 32,
            rgb: vec![[0.0; 3]; 32 * 32],
        };
        dot.rgb[16 * 32 + 16] = [100.0; 3];
        let glow = dot.bloom(0.1);
        assert!(glow.rgb[16 * 32 + 18][0] > 0.0);
        assert_eq!(glow.rgb[0][0], 0.0);
    }
}
