//! The atmosphere (#53) on a Vulkan device (lavapipe in CI): both views read the same tables; the
//! sky follows the local up and the sun's height, day and night; the Reference samples the
//! sun-free table in proportion to its brightness and weighs that against its bounce rays, and the
//! weights add up (a twin with the double-counting fault shows the test can fail); the tables
//! agree with their CPU twins.

mod common;

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};
use common::{PRIMARY, check_golden};
use materials::MaterialTable;
use planet_gen::generate_region;
use render::atmosphere::{Atmosphere, TableId, Tint, TintKey};
use render::sun::DEFAULT_ANGULAR_RADIUS;
use render::{Camera, Fault, Look, Offscreen, ReferenceOptions, Scene, SkyEstimator, Stages, Sun};

const STONE: Material = Material(1);

/// A small floor, so a ray that goes up never meets anything.
fn floor() -> Scene {
    let mut store = BrickStore::new();
    for x in -16..16 {
        for z in -16..16 {
            store.set(VoxelCoord([x, -1, z]), Some(STONE));
        }
    }
    Scene::new(&store, None)
}

/// The linear light of the middle column of a view that looks up at 45 degrees with a field of
/// view of 90, so that its rows run from the horizon (the bottom) to the zenith (the top): the
/// elevation of each of `rows` rows and its colour, from the horizon up.
fn sky_column(
    gpu: &mut Offscreen,
    look: Look,
    azimuth: f32,
    elevation: f32,
    rows: usize,
) -> Vec<(f64, [f32; 3])> {
    gpu.set_look(look);
    gpu.set_sun(Sun::from_angles(azimuth, elevation, DEFAULT_ANGULAR_RADIUS));
    let mut camera = Camera::looking_at([0.5, 40.0, 0.5], [100.0, 40.0 + 100.0, 0.5]);
    camera.fov_y = 90f64.to_radians();
    gpu.render(&camera);
    let ((width, height, hdr), _) = gpu.read_hdr_and_bloom();
    (0..rows)
        .rev()
        .map(|row| {
            let ndc = ((row as f64 + 0.5) / f64::from(height)) * 2.0 - 1.0;
            let elevation = 45.0 + (-ndc).atan().to_degrees();
            (elevation, hdr[row * width as usize + width as usize / 2])
        })
        .collect()
}

fn sky_gpu(rows: u32) -> Offscreen {
    let mut gpu = Offscreen::new(8, rows, &floor()).unwrap();
    gpu.set_stages(Stages::NONE);
    gpu
}

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

#[test]
fn the_sky_is_blue_overhead_and_warm_and_pale_at_the_horizon_by_day() {
    // The physical air's: the tint (below) is what makes the day's sky hazy and orange.
    let mut gpu = sky_gpu(16);
    let column = sky_column(
        &mut gpu,
        Look::HERO_B_UNTINTED,
        270f32.to_radians(),
        40f32.to_radians(),
        16,
    );
    let (_, low) = column[0];
    let (_, top) = column[15];
    assert!(top[2] > top[0] * 1.3, "blue overhead: {top:?}");
    assert!(
        low[0] > top[0] * 2.0 && low[1] > top[1],
        "brighter at the horizon: {low:?} {top:?}"
    );
}

#[test]
fn the_sky_follows_the_local_up_not_world_y() {
    // The same view of the sky from the top of a Planet (up +y) and from its side (up +x, the
    // camera carried 6.4 million Voxels out along it): the camera's frame and the sun, which
    // follows the local up (`set_local_sun`), make the two images the same, though the sky's
    // gradient runs along y in one and along x in the other.
    const GROUND: f64 = 320_000.0 * 20.0;
    let mut top = Camera::looking_at([0.5, 40.0, 0.5], [100.0, 90.0, 0.5]);
    top.fov_y = 80f64.to_radians();
    let mut side = top;
    side.position = [GROUND + 40.0, 0.0, 0.0];
    side.carry_up([1.0, 0.0, 0.0]);
    let render = |camera: &Camera, ground: f64| {
        let mut gpu = Offscreen::new(48, 32, &floor()).unwrap();
        gpu.set_stages(Stages::NONE);
        gpu.set_ground_level(ground);
        gpu.set_local_sun(1.0, 12f32.to_radians(), DEFAULT_ANGULAR_RADIUS);
        gpu.render(camera);
        gpu.read_hdr_and_bloom().0
    };
    let (w, h, a) = render(&top, 0.0);
    let (_, _, b) = render(&side, GROUND);
    let mut worst = 0.0f32;
    let mut sky_pixels = 0;
    for i in 0..(w * h) as usize {
        // Only pixels of the sky: the floor is where the Planet's surface was put, and the side
        // view has none.
        if luminance(a[i]) > 0.0 && luminance(b[i]) > 0.0 {
            sky_pixels += 1;
            worst =
                worst.max((luminance(a[i]) - luminance(b[i])).abs() / luminance(a[i]).max(0.02));
        }
    }
    assert!(sky_pixels > 1000);
    assert!(
        worst < 0.03,
        "the side's sky differs from the top's by {worst}"
    );
    // And that the world's y is not what the sky is a gradient of: in the side view the sky at the
    // top of the image is the zenith's colour, and it is bluer than at the horizon.
    let (top_px, bottom_px) = (
        b[(w * 2 + w / 2) as usize],
        b[(w * (h / 2) + w / 2) as usize],
    );
    assert!(
        top_px[2] / top_px[0] > bottom_px[2] / bottom_px[0],
        "{top_px:?} {bottom_px:?}"
    );
}

#[test]
fn day_follows_the_sun_and_night_is_dark() {
    let mut gpu = sky_gpu(8);
    let mut at = |elevation_degrees: f32| {
        let column = sky_column(
            &mut gpu,
            Look::HERO_B,
            270f32.to_radians(),
            elevation_degrees.to_radians(),
            8,
        );
        column
            .iter()
            .map(|(_, c)| luminance(*c))
            .fold(0.0, f32::max)
    };
    let noon = at(60.0);
    let low = at(8.0);
    let dusk = at(-3.0);
    let night = at(-30.0);
    assert!(
        noon > low && low > dusk && dusk > night,
        "{noon} {low} {dusk} {night}"
    );
    assert!(night < noon * 0.01, "night is dark: {night} against {noon}");
    // The sun's own light is gone with it.
    let air = Atmosphere::HERO;
    assert!(air.sun_light_on_ground(0.5)[0] > 1.0);
    assert_eq!(air.sun_light_on_ground(-0.3), [0.0; 3]);
}

#[test]
fn the_sunset_is_redder_at_the_horizon_than_the_noon_sky() {
    let mut gpu = sky_gpu(8);
    let ratio = |gpu: &mut Offscreen, look: Look, elevation: f32, azimuth: f32| {
        // The column looks along +x (azimuth 90): the sun at `azimuth`.
        let column = sky_column(gpu, look, azimuth.to_radians(), elevation.to_radians(), 8);
        let (_, c) = column[0];
        c[0] / c[2]
    };
    // The sun is behind the camera, which looks along +x. The air's own reddening:
    let noon = ratio(&mut gpu, Look::HERO_B_UNTINTED, 60.0, 270.0);
    let sunset = ratio(&mut gpu, Look::HERO_B_UNTINTED, 4.0, 270.0);
    assert!(sunset > noon * 1.5, "reddening: {sunset} against {noon}");
    // With the tint, which makes the noon's sky an orange of its own, the low sun's horizon is
    // still the redder end of its sky: warmer than the sky overhead.
    let column = sky_column(
        &mut gpu,
        Look::HERO_B,
        270f32.to_radians(),
        8f32.to_radians(),
        8,
    );
    let (low, high) = (column[0].1, column[7].1);
    assert!(
        low[0] / low[2] > 2.0 * high[0] / high[2],
        "warmer at the horizon, tinted: {low:?} against {high:?}"
    );
}

/// The sun-free table's weights and its distribution, from the table itself.
#[test]
fn the_sun_free_table_has_no_sun_and_its_distribution_is_its_brightness() {
    let mut gpu = sky_gpu(8);
    sky_column(
        &mut gpu,
        Look::HERO_B,
        270f32.to_radians(),
        8f32.to_radians(),
        8,
    );
    let table = gpu.read_atmosphere_table(TableId::SunFreeSky);
    let cdf = gpu.read_sky_distribution();
    let (w, h) = (64usize, 32usize);
    assert_eq!(table.len(), w * h);
    // A texel's weight is its luminance (and a little more, so that no direction is never chosen).
    let weight = |i: usize| luminance(table[i]).max(0.0) + 1e-7;
    let total: f32 = (0..w * h).map(weight).sum();
    let reported = cdf[h + h * w];
    assert!(
        (reported - total).abs() / total < 1e-3,
        "{reported} against {total}"
    );
    let mut above = 0.0f32;
    for j in 0..h {
        let row: f32 = (0..w).map(|i| weight(j * w + i)).sum();
        above += row;
        assert!(
            (cdf[j] - above / total).abs() < 2e-4,
            "row {j}: {} against {}",
            cdf[j],
            above / total
        );
        let mut run = 0.0f32;
        for i in 0..w {
            run += weight(j * w + i);
            let got = cdf[h + j * w + i];
            assert!(
                (got - run / row).abs() < 2e-4,
                "texel {i},{j}: {got} against {}",
                run / row
            );
        }
        assert_eq!(cdf[h + j * w + w - 1], 1.0, "row {j} ends at 1");
    }
    assert_eq!(cdf[h - 1], 1.0);
    // No sun in it: the brightest texel is within a small factor of the mean. The sun's disc, which
    // the Reference counts by a shadow ray alone, would be thousands of times that.
    let brightest = (0..w * h).map(|i| luminance(table[i])).fold(0.0, f32::max);
    let mean = total / (w * h) as f32;
    assert!(
        brightest < 8.0 * mean,
        "{brightest} against a mean of {mean}"
    );
}

#[test]
fn the_transmittance_table_is_the_cpu_twin() {
    let mut gpu = sky_gpu(8);
    sky_column(
        &mut gpu,
        Look::HERO_B,
        270f32.to_radians(),
        8f32.to_radians(),
        8,
    );
    let table = gpu.read_atmosphere_table(TableId::Transmittance);
    let air = Atmosphere::HERO;
    let (w, h) = (256usize, 64usize);
    let (bottom, top) = (
        f64::from(air.ground_radius),
        f64::from(air.ground_radius + air.top_height),
    );
    let mut compared = 0;
    for (x, y) in [
        (10, 3),
        (100, 5),
        (200, 20),
        (255, 40),
        (60, 63),
        (30, 2),
        (180, 50),
    ] {
        // Bruneton's mapping, as transmittance_params does.
        let (ux, uy) = (x as f64 / (w - 1) as f64, y as f64 / (h - 1) as f64);
        let hh = ((top - bottom) * (top + bottom)).sqrt();
        let rho = hh * uy;
        let r = (rho * rho + bottom * bottom).sqrt();
        let (d_min, d_max) = (top - r, rho + hh);
        let d = d_min + ux * (d_max - d_min);
        let mu = if d > 0.0 {
            ((hh * hh - rho * rho - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0)
        } else {
            1.0
        };
        // The table goes through the ground for a ray that meets it: only compare the rest.
        let s = (1.0 - mu * mu).sqrt();
        if mu < 0.0 && (bottom - r * s) * (bottom + r * s) >= 0.0 {
            continue;
        }
        let want = air.sun_transmittance((r - bottom) as f32, mu as f32);
        let got = table[y * w + x];
        for c in 0..3 {
            assert!(
                (got[c] - want[c]).abs() < 0.02 + 0.03 * want[c],
                "texel {x},{y} (r {r:.0}, mu {mu:.3}) channel {c}: {} against {}",
                got[c],
                want[c]
            );
        }
        compared += 1;
    }
    assert!(compared >= 4, "{compared}");
}

// ---- Goldens: the sky from the side of the Planet, a sunset from the ground, the rim from orbit.

const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;
/// The Planet's sea level, 320 km, in Voxels.
const GROUND: f64 = 320_000.0 * 20.0;

/// The view of `camera` as the game draws a still view: the display transform's stages on.
fn shown(gpu: &mut Offscreen, camera: &Camera) -> render::Image {
    gpu.render(camera)
}

/// A patch of floor at the side of the Planet, where "up" is +x: its top face at x = [`GROUND`].
fn side_floor() -> Scene {
    let mut store = BrickStore::new();
    let g = GROUND as i32;
    for x in g - 3..g {
        for y in -96..96 {
            for z in -96..96 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    Scene::new(&store, None)
}

#[test]
fn the_sky_from_the_side_of_the_planet_is_a_gradient_along_its_local_up() {
    // The camera is carried to the side of the Planet, where up is +x and world y is a direction
    // along the ground: the sky is still brightest at the horizon and bluest overhead, and the
    // sun is as high above the ground as it is from the top.
    let mut camera = Camera::looking_at([0.5, 40.0, 0.5], [60.0, 52.0, -40.0]);
    camera.position = [GROUND + 40.0, 0.0, 0.0];
    camera.carry_up([1.0, 0.0, 0.0]);
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &side_floor()).unwrap();
    gpu.set_ground_level(GROUND);
    gpu.set_local_sun(1.2, 14f32.to_radians(), DEFAULT_ANGULAR_RADIUS);
    check_golden(
        "atmosphere_side_256x144.png",
        &shown(&mut gpu, &camera),
        &PRIMARY,
    );
}

/// A patch of the test Planet near the +y pole (the same as `golden.rs`'s).
fn planet_patch() -> Scene {
    let store = generate_region(
        0xB4A7_A9AC,
        BrickCoord([-55, 170, -45]),
        BrickCoord([55, 196, 32]),
    );
    Scene::new(&store, None)
}

#[test]
fn a_sunset_from_the_ground_reddens_the_sky_towards_the_sun() {
    let camera = Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1538.0, -300.0]);
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &planet_patch()).unwrap();
    // The ground there is at y = 1535 Voxels.
    gpu.set_ground_level(1535.0);
    // The sun 2 degrees above the horizon, a quarter turn to the right of the view, a little ahead.
    gpu.set_sun(Sun::from_angles(
        -0.45,
        2.0f32.to_radians(),
        DEFAULT_ANGULAR_RADIUS,
    ));
    let image = shown(&mut gpu, &camera);
    // The sky near the sun is redder than the sky away from it.
    let at = |x: u32, y: u32| {
        let i = ((y * image.width + x) * 4) as usize;
        [image.rgba[i], image.rgba[i + 1], image.rgba[i + 2]].map(f32::from)
    };
    let (near, far) = (at(WIDTH - 24, 20), at(24, 20));
    assert!(
        near[0] / near[2].max(1.0) > far[0] / far[2].max(1.0),
        "{near:?} against {far:?}"
    );
    check_golden("atmosphere_sunset_256x144.png", &image, &PRIMARY);
}

#[test]
fn from_orbit_the_atmosphere_is_a_bright_rim_round_the_planet() {
    // 1.8 Planet radii from the centre, above the lit side, looking down at the limb (the edge of the
    // Planet, 34 degrees from straight down). The scene has no ground in it: what is drawn is the
    // air alone, the sky's light along rays that miss every brick, so a ray that grazes the Planet
    // passes through the most air and is brightest, and a ray that misses the air is black.
    let mut camera = Camera::looking_at([0.0, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    camera.position = [1.8 * GROUND, 0.0, 0.0];
    camera.carry_up([1.0, 0.0, 0.0]);
    camera.pitch = -(90.0f64 - 33.7).to_radians();
    camera.fov_y = 38f64.to_radians();
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &Scene::default()).unwrap();
    gpu.set_ground_level(GROUND);
    gpu.set_sun(Sun::from_angles(1.0, 0.4, DEFAULT_ANGULAR_RADIUS));
    let image = shown(&mut gpu, &camera);
    // The air's glow: some of the image is lit, and some (above the limb, in space) is black.
    let lit = |x: u32, y: u32| {
        let i = ((y * image.width + x) * 4) as usize;
        u32::from(image.rgba[i]) + u32::from(image.rgba[i + 1]) + u32::from(image.rgba[i + 2])
    };
    assert!(
        lit(WIDTH / 2, HEIGHT - 8) > 0,
        "the Planet's side is lit by the air"
    );
    assert_eq!(lit(WIDTH / 2, 4), 0, "space above the air is black");
    check_golden("atmosphere_orbit_256x144.png", &image, &PRIMARY);
}

// ---- The Reference's estimator for the sky.

/// A grey floor with a pillar, flat colours, in the dusk: the sun gives nothing (it is below the
/// horizon), so what a pixel gets is the sky's light, from bounce rays, the sky's own samples, or
/// both.
fn mean_of_the_reference(options: ReferenceOptions, samples: u32) -> [f64; 3] {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -4..0 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    for x in 6..12 {
        for z in -3..3 {
            for y in 0..24 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    let scene = Scene::new(&store, None);
    let mut gpu = Offscreen::new(16, 16, &scene).unwrap();
    gpu.set_stages(Stages::NONE);
    gpu.set_look(Look::HERO_B);
    // Twilight: the sky is bright, the sun is not (below the horizon the air lets none of it through).
    gpu.set_sun(Sun::from_angles(
        1.0,
        -2.0f32.to_radians(),
        DEFAULT_ANGULAR_RADIUS,
    ));
    gpu.set_reference_options(options);
    let camera = Camera::looking_at([-4.0, 14.0, 0.0], [4.0, 0.0, 0.0]);
    gpu.render_reference(&camera, samples);
    let accumulation = gpu.read_accumulation();
    let n = accumulation.len() / 3;
    let mut sum = [0.0f64; 3];
    for pixel in accumulation.chunks(3) {
        for c in 0..3 {
            sum[c] += f64::from(pixel[c]);
        }
    }
    sum.map(|v| v / n as f64)
}

fn opts(sky: SkyEstimator, fault: Fault) -> ReferenceOptions {
    ReferenceOptions {
        flat_colours: true,
        sky,
        fault,
        ..ReferenceOptions::default()
    }
}

#[test]
fn the_sky_samples_and_the_bounce_rays_weigh_to_the_same_light_as_either_alone() {
    const SAMPLES: u32 = 192;
    let both = mean_of_the_reference(opts(SkyEstimator::Both, Fault::None), SAMPLES);
    let bounce = mean_of_the_reference(opts(SkyEstimator::BounceOnly, Fault::None), SAMPLES);
    let samples = mean_of_the_reference(opts(SkyEstimator::SkySamplesOnly, Fault::None), SAMPLES);
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    assert!(lum(both) > 0.02, "there is light to count: {both:?}");
    for (name, other) in [
        ("bounce rays only", bounce),
        ("the sky's samples only", samples),
    ] {
        let d = (lum(both) - lum(other)).abs() / lum(both);
        assert!(
            d < 0.03,
            "{name}: {} against {} (both) differ by {d}",
            lum(other),
            lum(both)
        );
    }
}

#[test]
fn counting_the_sky_in_full_twice_is_caught_by_that_test() {
    const SAMPLES: u32 = 192;
    let honest = mean_of_the_reference(opts(SkyEstimator::Both, Fault::None), SAMPLES);
    let doubled = mean_of_the_reference(opts(SkyEstimator::Both, Fault::SkyDoubleCount), SAMPLES);
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let d = (lum(doubled) - lum(honest)) / lum(honest);
    assert!(
        d > 0.25,
        "the weights add up to 2 and the light is {d} over"
    );
}

// ---- Aerial perspective: the air between the camera and a surface, in both views.

const WALL_ROWS: u32 = 32;
const WALL_COLUMNS: u32 = 8;

/// A wall `distance` Voxels in front of the camera (at -z), stone on the left and grassy soil on the
/// right, filling the lower half of a view of 0.3 degrees: the camera is 2 m up, so the wall's top
/// edge is at the horizon.
fn wall_at(distance: i32) -> Scene {
    let soil = Material(MaterialTable::built_in().by_name("grassy soil").unwrap().id);
    let reach = (f64::from(distance) * 0.15f64.to_radians().tan() * 1.4) as i32 + 4;
    let mut store = BrickStore::new();
    for x in -reach..reach {
        for y in 40 - reach..40 {
            for z in -distance..-distance + 2 {
                store.set(
                    VoxelCoord([x, y, z]),
                    Some(if x < 0 { STONE } else { soil }),
                );
            }
        }
    }
    Scene::new(&store, None)
}

/// The wall's left half's luminance and how different the two halves look (the difference of their
/// luminances), in the view of Primary and of the Reference. Whatever the air adds is the same on
/// both halves, so what is left of the difference is what the air lets through.
fn the_wall(distance: i32) -> [(f32, f32); 2] {
    let mut gpu = Offscreen::new(WALL_COLUMNS, WALL_ROWS, &wall_at(distance)).unwrap();
    gpu.set_stages(Stages::NONE);
    gpu.set_sun(Sun::from_angles(0.0, 0.35, DEFAULT_ANGULAR_RADIUS));
    // On the line between the two halves, so that the columns at either side of the middle see one each.
    let mut camera = Camera::looking_at([0.0, 40.0, 0.5], [0.0, 40.0, -1000.0]);
    camera.fov_y = 0.3f64.to_radians();
    let row = WALL_ROWS - 3;
    let at = |column: u32| (row * WALL_COLUMNS + column) as usize;
    gpu.render(&camera);
    let ((_, _, hdr), _) = gpu.read_hdr_and_bloom();
    let (left, right) = (luminance(hdr[at(1)]), luminance(hdr[at(6)]));
    let primary = (left, (left - right).abs());
    gpu.render_reference(&camera, 128);
    let accumulation = gpu.read_accumulation();
    let lum = |column: u32| {
        luminance(
            accumulation[at(column) * 3..at(column) * 3 + 3]
                .try_into()
                .unwrap(),
        )
    };
    [primary, (lum(1), (lum(1) - lum(6)).abs())]
}

#[test]
fn the_air_takes_the_contrast_of_a_distant_wall_and_leaves_the_same_light_in_both_views() {
    // 20 m, 400 m and 6 km.
    let walls = [400, 8_000, 120_000].map(the_wall);
    for (view, name) in [(0, "Primary"), (1, "the Reference")] {
        let [near, middle, far] = walls.map(|w| w[view].1);
        println!("{name}: contrast {near:.4} at 20 m, {middle:.4} at 400 m, {far:.6} at 6 km");
        assert!(
            near > 0.05,
            "{name}: a wall 20 m away shows what it is made of: {near}"
        );
        assert!(
            middle < 0.6 * near && far < 0.01 * near,
            "{name}: the air takes it: {near} {middle} {far}"
        );
    }
    // With the wall's own light gone, the light that is left is the air's, and it is the same air in
    // both views (the two views light a wall differently, which is what the contrast of the near wall
    // shows, but not what the air adds).
    let (primary, reference) = (walls[2][0].0, walls[2][1].0);
    assert!(
        (primary - reference).abs() < 0.02 * primary,
        "{primary} (Primary) against {reference} (the Reference)"
    );
}

// ---- The hero tint (the owner's decision of 2026-11-18): in the tables, so in both views.

/// The wall of [`wall_at`] `metres` away, in the colour of its left half in the view of Primary and
/// of the Reference, linear, under `look`.
fn far_wall(metres: i32, look: Look) -> [[f32; 3]; 2] {
    let mut gpu = Offscreen::new(WALL_COLUMNS, WALL_ROWS, &wall_at(metres * 20)).unwrap();
    gpu.set_stages(Stages::NONE);
    let primary = wall_colour(&mut gpu, look);
    let mut camera = Camera::looking_at([0.0, 40.0, 0.5], [0.0, 40.0, -1000.0]);
    camera.fov_y = 0.3f64.to_radians();
    gpu.render_reference(&camera, 64);
    let accumulation = gpu.read_accumulation();
    let at = ((WALL_ROWS - 3) * WALL_COLUMNS + 1) as usize * 3;
    [primary, accumulation[at..at + 3].try_into().unwrap()]
}

/// The sky straight ahead, level, from the wall's camera (looking along -z), linear, under `look`,
/// with the wall's sun: what a far wall's haze fades to.
fn level_sky(look: Look) -> [f32; 3] {
    let mut gpu = Offscreen::new(8, 8, &floor()).unwrap();
    gpu.set_stages(Stages::NONE);
    gpu.set_look(look);
    gpu.set_sun(Sun::from_angles(
        0.0,
        8f32.to_radians(),
        DEFAULT_ANGULAR_RADIUS,
    ));
    let mut camera = Camera::looking_at([0.0, 40.0, 0.5], [0.0, 40.0, -1000.0]);
    camera.fov_y = 0.3f64.to_radians();
    gpu.render(&camera);
    let ((_, _, hdr), _) = gpu.read_hdr_and_bloom();
    hdr[3 * 8 + 1]
}

#[test]
fn the_hero_tint_fades_a_far_wall_into_the_sky_behind_it_in_both_views() {
    // 3 km: the wall's own light is gone, and what is seen is the haze, which is the sky in that
    // direction (so that no line is left between the ground and the sky), by the same function in
    // both views.
    let sky = level_sky(Look::HERO_B);
    let [primary, reference] = far_wall(3_000, Look::HERO_B);
    for (name, seen) in [("Primary", primary), ("the Reference", reference)] {
        for c in 0..3 {
            let d = (seen[c] - sky[c]).abs() / sky[c];
            assert!(d < 0.08, "{name}: {seen:?} against the sky {sky:?}");
        }
    }
    // The untinted sky is another colour, which the tint changes.
    let untinted = level_sky(Look::HERO_B_UNTINTED);
    let apart = (0..3).any(|c| (untinted[c] - sky[c]).abs() / sky[c] > 0.15);
    assert!(apart, "{untinted:?} {sky:?}");
}

#[test]
fn the_tint_warms_the_horizon_and_blues_the_zenith_and_leaves_the_physical_tables_alone() {
    let mut gpu = sky_gpu(16);
    let away = (90.0f32 + 70.0).to_radians();
    let sun = 8f32.to_radians();
    let untinted = sky_column(&mut gpu, Look::HERO_B_UNTINTED, away, sun, 16);
    let tinted = sky_column(&mut gpu, Look::HERO_B, away, sun, 16);
    let (low_u, low_t) = (untinted[0].1, tinted[0].1);
    let (high_u, high_t) = (untinted[15].1, tinted[15].1);
    assert!(
        low_t[0] / low_t[2] > 1.5 * low_u[0] / low_u[2],
        "{low_t:?} against {low_u:?}"
    );
    assert!(
        high_t[2] / high_t[0] > high_u[2] / high_u[0],
        "{high_t:?} against {high_u:?}"
    );
    // The tint is not in the tables of the air itself: the transmittance table, which the sun's colour
    // on the ground comes from, is the same.
    let mut a = Offscreen::new(8, 8, &floor()).unwrap();
    let mut b = Offscreen::new(8, 8, &floor()).unwrap();
    let camera = Camera::looking_at([0.5, 40.0, 0.5], [100.0, 140.0, 0.5]);
    a.set_look(Look::HERO_B_UNTINTED);
    b.set_look(Look::HERO_B);
    a.render(&camera);
    b.render(&camera);
    assert_eq!(
        a.read_atmosphere_table(TableId::Transmittance),
        b.read_atmosphere_table(TableId::Transmittance)
    );
    assert_eq!(Atmosphere::PHYSICAL.tint, Tint::NONE);
    assert_ne!(Atmosphere::HERO.tint, Tint::NONE);
    assert_eq!(
        Atmosphere::HERO.sun_light_on_ground(0.14),
        Atmosphere::PHYSICAL.sun_light_on_ground(0.14)
    );
}

// ---- The tint follows the time of day (the owner's direction of 2026-11-18).

#[test]
fn the_tint_blends_smoothly_between_its_settings_as_the_sun_moves() {
    let tint = Atmosphere::HERO.tint;
    // At each setting's own height of the sun, that setting.
    let parts = |k: TintKey| {
        let mut v = Vec::new();
        v.extend(k.sky_horizon);
        v.extend(k.sky_zenith);
        v.push(k.haze_per_metre * 300.0);
        v.push(k.pastel);
        v
    };
    for (key, elevation) in tint.keys.iter().zip(Tint::ELEVATIONS) {
        let got = parts(tint.at(elevation.to_radians().sin()));
        for (a, b) in got.iter().zip(parts(*key)) {
            assert!((a - b).abs() < 1e-3, "{elevation}: {got:?}");
        }
    }
    // Beyond the first and the last, the same as they are.
    assert_eq!(tint.at(-1.0), tint.keys[0]);
    assert_eq!(tint.at(1.0), tint.keys[4]);
    // No pops: over a sweep of the sun in steps of a hundredth of a degree, no part of the tint
    // moves by more than a hundredth of its own size.
    let mut last = parts(tint.at((-20.0f32).to_radians().sin()));
    let mut moved = 0.0f32;
    for step in 1..=9000 {
        let elevation = -20.0 + step as f32 * 0.01;
        let now = parts(tint.at(elevation.to_radians().sin()));
        for (a, b) in last.iter().zip(&now) {
            assert!(
                (a - b).abs() < 0.01,
                "pop at {elevation} degrees: {a} to {b}"
            );
            moved += (a - b).abs();
        }
        last = now;
    }
    assert!(moved > 1.0, "the tint moves with the sun: {moved}");
    // Its direction: violet in the dark, hazier and more orange in the sun.
    let [nr, ng, nb] = tint.keys[0].sky_horizon;
    assert!(nb > nr && nr > ng, "violet at night: {:?}", tint.keys[0]);
    let [dr, dg, db] = tint.keys[4].sky_zenith;
    assert!(dr > dg && dg > db, "warm by day: {:?}", tint.keys[4]);
    assert!(
        tint.keys[4].haze_per_metre > tint.keys[3].haze_per_metre
            && tint.keys[3].haze_per_metre > tint.keys[2].haze_per_metre
            && tint.keys[2].haze_per_metre > tint.keys[0].haze_per_metre,
        "hazier as the sun climbs"
    );
    // The physical air has none of it, at any height of the sun.
    assert_eq!(Tint::NONE.at(0.3), TintKey::NONE);
    assert_eq!(Tint::NONE.at(-0.3), TintKey::NONE);
}

#[test]
fn the_sky_is_violet_in_the_dark_and_hazy_orange_in_the_sun() {
    let mut gpu = sky_gpu(16);
    let away = (90.0f32 + 70.0).to_radians();
    let mut at = |look: Look, elevation: f32| {
        let column = sky_column(&mut gpu, look, away, elevation.to_radians(), 16);
        (column[0].1, column[15].1)
    };
    // Night: the sky overhead and at the horizon leans to blue and red over green.
    let (low, top) = at(Look::HERO_B, -12.0);
    for c in [low, top] {
        assert!(c[2] > c[0] && c[0] > c[1] * 1.2, "violet at night: {c:?}");
    }
    // Midday: not the blue overhead of the physical air: warm, and an orange horizon.
    let (low, top) = at(Look::HERO_B, 60.0);
    assert!(top[0] > top[2] * 1.15, "warm overhead by day: {top:?}");
    assert!(
        low[0] > low[2] * 1.8 && low[0] > low[1] * 1.4,
        "orange horizon: {low:?}"
    );
    let (_, physical_top) = at(Look::HERO_B_UNTINTED, 60.0);
    assert!(
        physical_top[2] > physical_top[0],
        "it is the tint: {physical_top:?}"
    );
    // Between, the tint moves a little at a time: what it does to the sky at the horizon (the tinted
    // sky over the physical one), in steps of a degree of the sun across the whole range, never
    // changes by more than a fifth of itself.
    let mut gain = |elevation: f32| {
        let (tinted, _) = at(Look::HERO_B, elevation);
        let (physical, _) = at(Look::HERO_B_UNTINTED, elevation);
        std::array::from_fn::<f32, 3, _>(|c| tinted[c] / physical[c])
    };
    let mut last = gain(-15.0);
    for step in 1..=75 {
        let now = gain(-15.0 + step as f32);
        for c in 0..3 {
            let rel = (now[c] - last[c]).abs() / last[c];
            assert!(rel < 0.2, "pop at {} degrees: {last:?} {now:?}", -15 + step);
        }
        last = now;
    }
}

// ---- Haze on the bounce rays (ADR 0005's bias ledger): leaving it off is worth under 1 %.

/// The linear light of the Reference view of `camera` over the test Planet's patch, 64 samples a
/// pixel, with `bounce_haze` on or off.
fn reference_light(camera: &Camera, sun: Sun, bounce_haze: bool) -> Vec<f32> {
    let mut gpu = Offscreen::new(96, 54, &planet_patch()).unwrap();
    gpu.set_stages(Stages::NONE);
    gpu.set_ground_level(1535.0);
    gpu.set_sun(sun);
    gpu.set_reference_options(ReferenceOptions {
        bounce_haze,
        ..ReferenceOptions::default()
    });
    gpu.render_reference(camera, 64);
    gpu.read_accumulation()
}

/// How much of a pixel's light the air along its bounce rays is worth, by the view: the mean of
/// |with - without| / without over the pixels, the 99th percentile of the same, and the whole
/// image's share.
fn haze_on_bounce_rays(camera: &Camera, sun: Sun) -> (f64, f64, f64) {
    let without = reference_light(camera, sun, false);
    let with = reference_light(camera, sun, true);
    let (mut sum, mut total, mut moved) = (0.0, 0.0, 0.0);
    let mut each = Vec::new();
    for (a, b) in without.chunks(3).zip(with.chunks(3)) {
        let (a, b) = (f64::from(a[0] + a[1] + a[2]), f64::from(b[0] + b[1] + b[2]));
        total += a;
        moved += (b - a).abs();
        if a > 1e-3 {
            each.push((b - a).abs() / a);
            sum += (b - a).abs() / a;
        }
    }
    each.sort_by(f64::total_cmp);
    (
        sum / each.len() as f64,
        each[each.len() * 99 / 100],
        moved / total,
    )
}

#[test]
fn leaving_haze_off_the_bounce_rays_is_worth_under_one_percent_of_a_pixels_light() {
    let views = [
        // The golden's views: low over the ground looking at the horizon, and close to the
        // terraced hillside with the sun low to the side.
        (
            Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]),
            Sun::from_angles(0.9, 0.14, DEFAULT_ANGULAR_RADIUS),
        ),
        (
            Camera::looking_at([0.0, 1535.0, 120.0], [0.0, 1508.0, 20.0]),
            Sun::from_angles(0.9, 0.25, 0.04),
        ),
        (
            Camera::looking_at([0.0, 1620.0, 300.0], [0.0, 1500.0, -300.0]),
            Sun::from_angles(2.0, 0.14, DEFAULT_ANGULAR_RADIUS),
        ),
        // The tint's densest haze (midday, a sun 60 degrees up) and its dusk (just under the
        // horizon): the tint follows the sun, and so does the haze the bound is for.
        (
            Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]),
            Sun::from_angles(0.9, 60f32.to_radians(), DEFAULT_ANGULAR_RADIUS),
        ),
        (
            Camera::looking_at([0.0, 1620.0, 300.0], [0.0, 1500.0, -300.0]),
            Sun::from_angles(2.0, -4f32.to_radians(), DEFAULT_ANGULAR_RADIUS),
        ),
    ];
    for (camera, sun) in views {
        let (mean, p99, whole) = haze_on_bounce_rays(&camera, sun);
        println!(
            "haze on bounce rays: pixel mean {mean:.5}, 99th percentile {p99:.5}, whole image {whole:.5}"
        );
        assert!(mean < 0.01, "the mean pixel moves by {mean}");
        assert!(p99 < 0.01, "the 99th percentile pixel moves by {p99}");
        assert!(whole < 0.01, "the image moves by {whole}");
    }
}

// ---- Tuning (an exploration, not a test: `cargo test -p render --test atmosphere tune -- --ignored
// --nocapture` searches the air's parameters for hero light B's look and prints what it found).

const HORIZON: [f32; 3] = [1.0, 0.55, 0.38];
const ZENITH: [f32; 3] = [0.12, 0.18, 0.45];
const HERO_SUN: [f32; 3] = [1.3, 0.80, 0.40];

fn air_of(p: &[f64]) -> Atmosphere {
    let mut air = Atmosphere::PHYSICAL;
    // The Rayleigh scattering keeps Earth's blue-to-red ratio to within 40 % either way.
    let tilt = |c: usize| p[1 + c].clamp(-0.5, 0.5);
    air.rayleigh =
        std::array::from_fn(|c| Atmosphere::PHYSICAL.rayleigh[c] * (p[0] + tilt(c)).exp() as f32);
    air.ozone = Atmosphere::PHYSICAL.ozone.map(|c| c * p[4].exp() as f32);
    air.mie_height = p[5].exp() as f32;
    air.mie_g = (p[6].tanh() * 0.95) as f32;
    air.sun_light = std::array::from_fn(|c| (p[7] + p[10 + c].clamp(-0.5, 0.5)).exp() as f32);
    air.ground_albedo = [(p[8].exp() as f32).min(0.5); 3];
    air.rayleigh_height = p[9].exp() as f32;
    air
}

fn loss(gpu: &mut Offscreen, p: &[f64]) -> (f64, String) {
    let air = air_of(p);
    let look = Look {
        atmosphere: Some(air),
        ..Look::HERO_B
    };
    let mut total = 0.0;
    let mut report = String::new();
    for (away, weight) in [(180.0_f32, 1.0), (90.0, 0.7)] {
        let azimuth = (90.0 + away).to_radians();
        let col = sky_column(gpu, look, azimuth, 8f32.to_radians(), 16);
        let mut line = format!("  sun {away:3.0} off:");
        for (i, (e, c)) in col.iter().enumerate() {
            let h = (e.to_radians().sin()) as f32;
            let t: [f32; 3] = std::array::from_fn(|k| HORIZON[k] + (ZENITH[k] - HORIZON[k]) * h);
            for k in 0..3 {
                let d = f64::from(((c[k] + 0.02) / (t[k] + 0.02)).ln());
                total += weight * d * d / col.len() as f64;
            }
            if i % 3 == 0 {
                line += &format!(" [{e:.0}: {:.2} {:.2} {:.2}]", c[0], c[1], c[2]);
            }
        }
        report += &line;
        report.push('\n');
    }
    let sun = air.sun_light_on_ground(8f32.to_radians().sin());
    for k in 0..3 {
        let d = f64::from((sun[k] / HERO_SUN[k]).ln());
        total += 3.0 * d * d;
    }
    report += &format!(
        "  sun on ground {:.2} {:.2} {:.2} (hero {:?})\n",
        sun[0], sun[1], sun[2], HERO_SUN
    );
    (total, report)
}

#[test]
#[ignore = "exploration"]
fn tune() {
    let mut gpu = Offscreen::new(8, 16, &floor()).unwrap();
    gpu.set_stages(Stages::NONE);
    let start = std::env::var("TUNE_START").ok().map(|s| {
        s.split(',')
            .map(|v| v.parse::<f64>().unwrap())
            .collect::<Vec<_>>()
    });
    let mut p = start.unwrap_or_else(|| {
        vec![
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            60.0_f64.ln(),
            (0.7_f64 / 0.95).atanh(),
            2.0_f64.ln(),
            0.3_f64.ln(),
            8000.0_f64.ln(),
            0.0,
            0.0,
            0.0,
        ]
    });
    let (mut best, _) = loss(&mut gpu, &p);
    let mut step = vec![
        0.4, 0.2, 0.2, 0.2, 0.4, 0.3, 0.3, 0.2, 0.3, 0.2, 0.2, 0.2, 0.2,
    ];
    let mut rng = 12345u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    let iterations: usize = std::env::var("TUNE_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    for it in 0..iterations {
        let k = (next() * p.len() as f64) as usize;
        let sign = if next() < 0.5 { -1.0 } else { 1.0 };
        let mut q = p.clone();
        q[k] += sign * step[k] * (0.3 + next());
        let (l, _) = loss(&mut gpu, &q);
        if l < best {
            best = l;
            p = q;
        } else {
            step[k] *= 0.93;
        }
        if it % 10 == 0 {
            println!("it {it}: loss {best:.4}");
        }
    }
    let (l, report) = loss(&mut gpu, &p);
    println!("loss {l:.4}\n{report}");
    let joined: Vec<String> = p.iter().map(|v| format!("{v:.5}")).collect();
    println!("TUNE_START={}", joined.join(","));
    let air = air_of(&p);
    println!("{air:#?}");
}

#[test]
#[ignore = "exploration"]
fn sunset_ratios() {
    let mut gpu = sky_gpu(8);
    for elevation in [60.0f32, 20.0, 8.0, 4.0, 2.0, 1.0, 0.0] {
        for azimuth in [100.0f32, 180.0, 270.0] {
            let column = sky_column(
                &mut gpu,
                Look::HERO_B,
                azimuth.to_radians(),
                elevation.to_radians(),
                8,
            );
            let (_, c) = column[0];
            let (_, z) = column[7];
            println!(
                "sun {elevation:4.0} az {azimuth:3.0}: horizon {:.3} {:.3} {:.3} (R/B {:.2})  zenith {:.3} {:.3} {:.3}",
                c[0],
                c[1],
                c[2],
                c[0] / c[2],
                z[0],
                z[1],
                z[2]
            );
        }
    }
}

/// A wall's left-half colour as Primary draws it, linear, under `look`.
fn wall_colour(gpu: &mut Offscreen, look: Look) -> [f32; 3] {
    gpu.set_look(look);
    gpu.set_sun(Sun::from_angles(
        0.0,
        8f32.to_radians(),
        DEFAULT_ANGULAR_RADIUS,
    ));
    let mut camera = Camera::looking_at([0.0, 40.0, 0.5], [0.0, 40.0, -1000.0]);
    camera.fov_y = 0.3f64.to_radians();
    gpu.render(&camera);
    let ((_, _, hdr), _) = gpu.read_hdr_and_bloom();
    hdr[((WALL_ROWS - 3) * WALL_COLUMNS + 1) as usize]
}

#[test]
#[ignore = "exploration"]
fn dump_the_tint_views() {
    let out = std::path::PathBuf::from("/tmp/tint-views");
    std::fs::create_dir_all(&out).unwrap();
    let views = [
        (
            "surface",
            Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]),
        ),
        (
            "hillside",
            Camera::looking_at([0.0, 1535.0, 120.0], [0.0, 1508.0, 20.0]),
        ),
        (
            "high",
            Camera::looking_at([0.0, 1620.0, 300.0], [0.0, 1500.0, -300.0]),
        ),
    ];
    let looks = [
        ("1-gradient", Look::HERO_B_GRADIENT),
        ("2-untinted", Look::HERO_B_UNTINTED),
        ("3-tinted", Look::HERO_B),
    ];
    for (name, camera) in &views {
        for el in [-12.0f32, -4.0, 0.0, 8.0, 25.0, 60.0] {
            let sun = Sun::from_angles(1.5, el.to_radians(), DEFAULT_ANGULAR_RADIUS);
            for (slug, look) in looks {
                let mut gpu = Offscreen::new(320, 180, &planet_patch()).unwrap();
                gpu.set_ground_level(1535.0);
                gpu.set_look(look);
                gpu.set_sun(sun);
                let image = gpu.render_reference(camera, 32);
                common::write_png(&out.join(format!("{name}-{el:+03}-{slug}.png")), &image);
            }
        }
    }
}

#[test]
#[ignore = "exploration"]
fn sky_by_sun_elevation() {
    let mut gpu = sky_gpu(16);
    let away = (90.0f32 + 70.0).to_radians();
    for el in [-12.0f32, -8.0, -4.0, 0.0, 4.0, 8.0, 15.0, 25.0, 40.0, 60.0] {
        for (name, look) in [
            ("untinted", Look::HERO_B_UNTINTED),
            ("tinted", Look::HERO_B),
        ] {
            let col = sky_column(&mut gpu, look, away, el.to_radians(), 16);
            let (h, m, z) = (col[0].1, col[8].1, col[15].1);
            println!(
                "sun {el:5.0} {name:9}: horizon {:.3} {:.3} {:.3}  mid {:.3} {:.3} {:.3}  zenith {:.3} {:.3} {:.3}",
                h[0], h[1], h[2], m[0], m[1], m[2], z[0], z[1], z[2]
            );
        }
    }
}

#[test]
#[ignore = "exploration"]
fn horizon_profile() {
    let mut gpu = sky_gpu(96);
    gpu.set_stages(Stages::NONE);
    for el in [-12.0f32, -4.0, 8.0, 60.0] {
        for (name, look) in [
            ("untinted", Look::HERO_B_UNTINTED),
            ("tinted", Look::HERO_B),
        ] {
            gpu.set_look(look);
            gpu.set_sun(Sun::from_angles(
                (90.0f32 + 70.0).to_radians(),
                el.to_radians(),
                DEFAULT_ANGULAR_RADIUS,
            ));
            // Looking along +x, level, with a 12 degree field: rows cover -6 to +6 degrees.
            let mut camera = Camera::looking_at([0.5, 40.0, 0.5], [100.0, 40.0, 0.5]);
            camera.fov_y = 12f64.to_radians();
            gpu.render(&camera);
            let ((width, height, hdr), _) = gpu.read_hdr_and_bloom();
            println!("sun {el} {name}");
            for row in (0..height as usize).step_by(4) {
                let c = hdr[row * width as usize + width as usize / 2];
                let ndc = ((row as f64 + 0.5) / f64::from(height)) * 2.0 - 1.0;
                let e = (-ndc * (6f64.to_radians().tan())).atan().to_degrees();
                println!("  {e:+6.2} deg  {:.4} {:.4} {:.4}", c[0], c[1], c[2]);
            }
        }
    }
}

/// The limb at the zoom of ADR 0008 (1.8 to 20 Planet radii): the air's glow is brightest at the
/// edge of the Planet, fades into space over the tens of kilometres of the air above it, and the
/// brightness does not step. (The soft horizon of a camera near the ground holds the sky short of
/// the horizon by an angle, which would hold the limb's tens of kilometres at 20 radii if it did
/// not fade out with height.)
#[test]
fn the_limb_is_a_lit_glow_at_long_zoom_too() {
    for (radii, fov, limb) in [(10.0f64, 12.0f64, 5.74f64), (20.0, 6.0, 2.87)] {
        let mut camera = Camera::looking_at([0.0, 0.0, 0.0], [-1.0, 0.0, 0.0]);
        camera.position = [radii * GROUND, 0.0, 0.0];
        camera.carry_up([1.0, 0.0, 0.0]);
        camera.pitch = -(90.0f64 - limb).to_radians();
        camera.fov_y = fov.to_radians();
        let mut gpu = Offscreen::new(WIDTH, HEIGHT, &Scene::default()).unwrap();
        gpu.set_ground_level(GROUND);
        gpu.set_stages(Stages::NONE);
        gpu.set_sun(Sun::from_angles(1.0, 0.4, DEFAULT_ANGULAR_RADIUS));
        gpu.render(&camera);
        let ((w, h, hdr), _) = gpu.read_hdr_and_bloom();
        let column: Vec<f32> = (0..h)
            .map(|y| luminance(hdr[(y * w + w / 2) as usize]))
            .collect();
        let (peak_row, peak) =
            column.iter().enumerate().fold(
                (0, 0.0f32),
                |best, (y, &v)| if v > best.1 { (y, v) } else { best },
            );
        let bottom = column[h as usize - 1];
        assert_eq!(column[0], 0.0, "{radii} radii: space is black");
        assert!(
            peak > 1.8 * bottom,
            "{radii} radii: the limb is brightest: {peak} against {bottom}"
        );
        // Above the peak the glow fades into space over many rows, not a step: a few rows up it is
        // still over half the peak (held short of the horizon, it is a twentieth), and a dozen up it
        // is a fraction.
        let (near, far) = (column[peak_row - 6], column[peak_row - 12]);
        assert!(
            near > 0.45 * peak,
            "{radii} radii: the glow is not a step: {near} against {peak}"
        );
        assert!(
            far > 0.02 * peak && far < 0.4 * peak,
            "{radii} radii: the glow fades into space: {far} against {peak}"
        );
    }
}
