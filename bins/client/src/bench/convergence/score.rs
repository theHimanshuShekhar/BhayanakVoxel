//! Scoring the convergence phase's images, on the CPU, after the last sample is drawn: nothing
//! here runs between samples, so nothing here is in a timer.
//!
//! [`flip`] is the only function that calls `flip-rs`. Everything else is plain arithmetic on
//! numbers, so the tests of this module (and so CI) never run FLIP.

use flip_rs::{RgbImage, Weighting, ldr_flip};

/// Pixels per degree that FLIP is told the image is seen at: a 24 inch 4K display at 0.7 m, the
/// reference tool's default. Pinned with the seeds (ADR 0005 and #99 use it).
pub const FLIP_PPD: f32 = 67.02;

/// FLIP's mean and 99th percentile over a whole image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flip {
    pub mean: f64,
    /// The error-weighted 99th percentile, as the reference tool reports it: a guard against
    /// speckles that a mean hides.
    pub p99: f64,
}

/// LDR-FLIP of `test` against `reference`: both RGB, 8 bits a channel, `width` by `height`, rows
/// from the top, in the encoding the player sees.
pub fn flip(
    reference: &[u8],
    test: &[u8],
    (width, height): (usize, usize),
) -> Result<Flip, String> {
    let image = |pixels: &[u8]| {
        RgbImage::new(width, height, pixels.to_vec()).map_err(|e| format!("FLIP: {e}"))
    };
    let map = ldr_flip(&image(reference)?, &image(test)?, FLIP_PPD)
        .map_err(|e| format!("FLIP failed: {e}"))?;
    let p99 = map
        .percentile(0.99, Weighting::Weighted)
        .map_err(|e| format!("FLIP's 99th percentile failed: {e}"))?;
    Ok(Flip {
        mean: f64::from(map.statistics().mean),
        p99: f64::from(p99),
    })
}

/// Mean squared difference per channel of two images of floats (the f32 accumulations, in linear
/// light).
pub fn mse(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "images of different sizes");
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(&a, &b)| {
            let d = f64::from(a) - f64::from(b);
            d * d
        })
        .sum();
    sum / a.len() as f64
}

/// The 8-bit RGB image that the old display curve, a gamma of 2.2, drew from this accumulation
/// (linear light, RGB a pixel): until #188 the player saw this, and the true sRGB curve replaced
/// it. Rounded to the nearest level, as the unorm texture the shader writes to does. It is only
/// for scoring the old curve beside the new one in the same run (`--flip-gamma-2-2`).
pub fn gamma22_rgb(linear: &[f32]) -> Vec<u8> {
    linear
        .iter()
        .map(|&v| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8)
        .collect()
}

/// The mean of the frames `next` makes (`frames` of them), in linear light, as the 8-bit image the
/// player would see: the average of what a still view of Primary draws, a frame after a frame, which
/// is its own mean (`Primary converged`, #55). Frames of one size, RGBA, 8 bits a channel.
pub fn mean_of_frames(
    frames: u32,
    mut next: impl FnMut() -> render::Image,
) -> Option<render::Image> {
    let decode: Vec<f32> = (0..=255u8)
        .map(|c| render::srgb_decode(f32::from(c) / 255.0))
        .collect();
    let mut sum: Vec<f32> = Vec::new();
    let mut size = (0, 0);
    for _ in 0..frames {
        let image = next();
        if sum.is_empty() {
            size = (image.width, image.height);
            sum = vec![0.0; image.rgba.len() / 4 * 3];
        }
        for (acc, px) in sum
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(image.rgba.as_chunks::<4>().0)
        {
            for c in 0..3 {
                acc[c] += decode[usize::from(px[c])];
            }
        }
    }
    if sum.is_empty() {
        return None;
    }
    let scale = 1.0 / frames as f32;
    let rgba = sum
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|acc| {
            let level =
                |v: f32| (render::srgb_encode((v * scale).clamp(0.0, 1.0)) * 255.0).round() as u8;
            [level(acc[0]), level(acc[1]), level(acc[2]), 255]
        })
        .collect();
    Some(render::Image {
        width: size.0,
        height: size.1,
        rgba,
    })
}

/// RGB bytes of an RGBA image, which is what FLIP reads.
pub fn rgb_of_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// How a line (a FLIP level) was found on a curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum How {
    /// The curve is at or under the line at the first checkpoint: the count is that checkpoint.
    AtFirstCheckpoint,
    /// Between two checkpoints, on the straight line through them with both axes logarithmic.
    Interpolated,
    /// Past the last checkpoint, on the line through the last two. An estimate, not a
    /// measurement: the curve never got there.
    Extrapolated,
    /// The curve is not falling at the end, so it never gets there.
    NotReached,
}

/// Where a curve first gets to a line: after how many samples (none if it does not), and how that
/// was worked out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reach {
    pub samples: Option<f64>,
    pub how: How,
}

/// Where the curve `points` of `(samples, error)`, in order of samples, first gets to `line`.
/// `None` for a curve with no points.
pub fn samples_to_reach(points: &[(f64, f64)], line: f64) -> Option<Reach> {
    let found = |samples, how| Some(Reach { samples, how });
    let &(first_samples, first_error) = points.first()?;
    if first_error <= line {
        return found(Some(first_samples), How::AtFirstCheckpoint);
    }
    for pair in points.windows(2) {
        let ((s0, e0), (s1, e1)) = (pair[0], pair[1]);
        if e0 > line && e1 <= line {
            // A zero error (identical images) has no logarithm: it is under any line, and the
            // line is met at the end of the interval.
            if e1 <= 0.0 {
                return found(Some(s1), How::Interpolated);
            }
            let t = (line.ln() - e0.ln()) / (e1.ln() - e0.ln());
            let at = (s0.ln() + t * (s1.ln() - s0.ln())).exp();
            return found(Some(at), How::Interpolated);
        }
    }
    if let [.., (s0, e0), (s1, e1)] = points
        && *e1 > 0.0
        && *e0 > 0.0
    {
        let slope = (e1.ln() - e0.ln()) / (s1.ln() - s0.ln());
        if slope < 0.0 {
            let at = s1.ln() + (line.ln() - e1.ln()) / slope;
            return found(Some(at.exp()), How::Extrapolated);
        }
    }
    found(None, How::NotReached)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_converged_image_is_the_mean_in_linear_light() {
        // What this guards: `Primary converged` averages the light, not the 8-bit codes. Frames of
        // black and white average to half the light, which the sRGB curve draws as 188 (not 128, the
        // mean of the codes); alpha stays 255. Without frames there is no image.
        let frame = |level: u8| render::Image {
            width: 2,
            height: 1,
            rgba: vec![level, level, level, 255, level, 0, 255 - level, 255],
        };
        let mut n = 0;
        let mean = mean_of_frames(4, || {
            n += 1;
            frame(if n % 2 == 0 { 255 } else { 0 })
        })
        .unwrap();
        assert_eq!((mean.width, mean.height), (2, 1));
        assert_eq!(&mean.rgba[..4], &[188, 188, 188, 255]);
        // The other pixel's green is 0 in every frame, its blue was 255 half of the time.
        assert_eq!(mean.rgba[5], 0);
        assert_eq!(mean.rgba[6], 188);
        assert!(mean_of_frames(0, || frame(0)).is_none());
    }

    #[test]
    fn the_mean_squared_difference_is_per_channel() {
        assert_eq!(mse(&[1.0, 2.0, 3.0, 4.0], &[1.0, 2.0, 3.0, 4.0]), 0.0);
        // Differences of 1 and 3 over four floats: (1 + 9) / 4.
        assert_eq!(mse(&[0.0, 0.0, 0.0, 0.0], &[1.0, 0.0, 3.0, 0.0]), 2.5);
    }

    #[test]
    fn the_old_curve_is_a_gamma_of_2_2_clamped_and_rounded() {
        // 0.5 ^ (1 / 2.2) = 0.7297, and the true sRGB curve gives 0.7354 there.
        assert_eq!(
            gamma22_rgb(&[0.0, 0.5, 1.0, 7.0, -1.0, 0.18]),
            [0, 186, 255, 255, 0, 117]
        );
    }

    #[test]
    fn the_alpha_of_an_image_is_dropped_for_flip() {
        assert_eq!(
            rgb_of_rgba(&[1, 2, 3, 255, 4, 5, 6, 255]),
            [1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn a_line_between_two_checkpoints_is_found_on_the_log_log_line() {
        // Errors 0.08 at 1 sample and 0.02 at 16: the error falls as 1/sqrt(n), so 0.04 is at 4.
        let points = [(1.0, 0.08), (16.0, 0.02)];
        let reach = samples_to_reach(&points, 0.04).unwrap();
        assert_eq!(reach.how, How::Interpolated);
        assert!((reach.samples.unwrap() - 4.0).abs() < 1e-9, "{reach:?}");
    }

    #[test]
    fn a_curve_that_starts_under_the_line_is_there_at_the_first_checkpoint() {
        assert_eq!(
            samples_to_reach(&[(1.0, 0.01), (4.0, 0.005)], 0.05),
            Some(Reach {
                samples: Some(1.0),
                how: How::AtFirstCheckpoint
            })
        );
    }

    #[test]
    fn a_curve_that_stops_short_is_extrapolated_and_says_so() {
        // 0.04 at 16 samples, 0.02 at 64: 0.01 is a halving of the error away, at 256.
        let reach = samples_to_reach(&[(16.0, 0.04), (64.0, 0.02)], 0.01).unwrap();
        assert_eq!(reach.how, How::Extrapolated);
        assert!((reach.samples.unwrap() - 256.0).abs() < 1e-6, "{reach:?}");
    }

    #[test]
    fn a_curve_that_is_not_falling_never_gets_there() {
        let reach = samples_to_reach(&[(16.0, 0.04), (64.0, 0.05)], 0.01).unwrap();
        assert_eq!(
            reach,
            Reach {
                samples: None,
                how: How::NotReached
            }
        );
    }

    #[test]
    fn a_zero_error_meets_every_line() {
        let reach = samples_to_reach(&[(1.0, 0.1), (4.0, 0.0)], 0.05).unwrap();
        assert_eq!(
            reach,
            Reach {
                samples: Some(4.0),
                how: How::Interpolated
            }
        );
    }

    #[test]
    fn no_points_have_no_line() {
        assert_eq!(samples_to_reach(&[], 0.05), None);
    }

    /// Runs FLIP, which CI must not (ADR 0005): `cargo test -p client --bin client -- --ignored
    /// flip_of`. It checks that the wrapper reads the images' size and bytes right, which the
    /// numbers of the bench rest on.
    #[test]
    #[ignore = "runs FLIP, which CI does not"]
    fn flip_of_an_image_against_itself_is_zero_and_against_another_is_not() {
        let size = (64, 64);
        let grey = vec![128u8; 64 * 64 * 3];
        let mut brighter = grey.clone();
        brighter.iter_mut().for_each(|v| *v = 200);
        let same = flip(&grey, &grey, size).unwrap();
        assert_eq!(
            same,
            Flip {
                mean: 0.0,
                p99: 0.0
            }
        );
        let different = flip(&grey, &brighter, size).unwrap();
        // A flat difference has one value everywhere: the mean and the percentile agree.
        assert!((different.mean - 0.578).abs() < 0.01, "{different:?}");
        assert!(
            (different.p99 - different.mean).abs() < 0.001,
            "{different:?}"
        );
        assert!(flip(&grey, &grey[..10], size).is_err());
    }
}
