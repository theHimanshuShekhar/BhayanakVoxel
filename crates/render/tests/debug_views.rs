//! The debug views on a hand-made scene, where each can be worked out. Needs a Vulkan device
//! (lavapipe in CI). That the default view is unchanged is the job of the golden images.

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;
use render::{Camera, DebugView, Image, Offscreen, Scene, Sun};

const STONE: Material = Material(1);
/// Rock in `materials.toml`.
const STONE_ALBEDO: [f64; 3] = [0.45, 0.43, 0.40];
const SIZE: u32 = 32;

/// A floor of `material`, Voxels y -8..=-1 (its top face is at y = 0), x and z -64..=63.
fn floor_of(material: Material) -> Scene {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(material));
            }
        }
    }
    Scene::new(&store, None)
}

fn floor() -> Scene {
    floor_of(STONE)
}

/// 20 Voxels above the floor, looking straight down.
fn overhead() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5])
}

fn skyward() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 40.0, 0.5])
}

fn gpu(view: DebugView) -> Offscreen {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor()).unwrap();
    gpu.set_debug_view(view);
    gpu
}

fn pixel(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * image.width + x) * 4) as usize;
    [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]]
}

fn near(actual: [u8; 3], expected: [f64; 3]) {
    for c in 0..3 {
        assert!(
            (f64::from(actual[c]) - expected[c]).abs() <= 2.0,
            "{actual:?} is not {expected:?}"
        );
    }
}

fn all_pixels(image: &Image) -> impl Iterator<Item = [u8; 3]> + '_ {
    (0..SIZE * SIZE).map(|i| pixel(image, i % SIZE, i / SIZE))
}

#[test]
fn normals_show_the_face_the_floor_presents_and_the_sky_is_black() {
    // The floor faces +y: (0, 1, 0) is (0.5, 1, 0.5) once scaled to 0..1.
    let image = gpu(DebugView::Normals).render(&overhead());
    assert!(all_pixels(&image).all(|p| {
        let d = |a: u8, b: f64| (f64::from(a) - b).abs() <= 2.0;
        d(p[0], 127.5) && d(p[1], 255.0) && d(p[2], 127.5)
    }));
    let sky = gpu(DebugView::Normals).render(&skyward());
    assert!(all_pixels(&sky).all(|p| p == [0; 3]));
}

#[test]
fn albedo_shows_the_material_without_light_or_shade() {
    let image = gpu(DebugView::Albedo).render(&overhead());
    let encoded = STONE_ALBEDO.map(|c| c.powf(1.0 / 2.2) * 255.0);
    assert!(
        all_pixels(&image).all(|p| (0..3).all(|c| (f64::from(p[c]) - encoded[c]).abs() <= 2.0))
    );
}

#[test]
fn depth_is_brighter_the_nearer_the_hit() {
    let image = gpu(DebugView::Depth).render(&overhead());
    // The middle looks straight down from 20 Voxels: 64 / (64 + 20) of full brightness.
    near(pixel(&image, 15, 15), [64.0 / 84.0 * 255.0; 3]);
    // A corner is further along its ray.
    assert!(pixel(&image, 0, 0)[0] < pixel(&image, 15, 15)[0]);
    let sky = gpu(DebugView::Depth).render(&skyward());
    assert!(all_pixels(&sky).all(|p| p == [0; 3]));
}

#[test]
fn the_sample_count_view_brightens_as_samples_accumulate() {
    let mut gpu = gpu(DebugView::SampleCount);
    // One sample: the dark end of the scale, (0.10, 0, 0.30).
    let one = gpu.render_reference(&overhead(), 1);
    near(pixel(&one, 3, 3), [25.5, 0.0, 76.5]);
    // Sixteen in all: 4 of the 10 doublings to 1024, so 0.4 of the way to (1, 0.9, 0.2).
    let sixteen = gpu.render_reference(&overhead(), 15);
    near(
        pixel(&sixteen, 3, 3),
        [255.0 * 0.46, 255.0 * 0.36, 255.0 * 0.26],
    );
    // The whole image shows one count, sky or not.
    assert!(all_pixels(&sixteen).all(|p| p == pixel(&sixteen, 3, 3)));
}

#[test]
fn a_debug_view_changes_the_picture_and_switching_back_restores_the_lit_image() {
    let mut gpu = gpu(DebugView::Lit);
    let lit = gpu.render(&overhead());
    gpu.set_debug_view(DebugView::Normals);
    assert_ne!(gpu.render(&overhead()), lit);
    gpu.set_debug_view(DebugView::Lit);
    assert_eq!(gpu.render(&overhead()), lit);
}

#[test]
fn the_reference_image_keeps_accumulating_while_a_debug_view_is_shown() {
    let mut a = gpu(DebugView::Lit);
    a.render_reference(&overhead(), 4);
    a.set_debug_view(DebugView::Depth);
    a.render_reference(&overhead(), 4);
    a.set_debug_view(DebugView::Lit);
    let switched = a.render_reference(&overhead(), 1);

    let straight = gpu(DebugView::Lit).render_reference(&overhead(), 9);
    assert_eq!(switched, straight);
}

fn material_id(name: &str) -> Material {
    Material(MaterialTable::built_in().by_name(name).unwrap().id)
}

/// A `name` floor seen from `overhead()` in the given view.
fn of_floor(name: &str, view: DebugView) -> Image {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material_id(name))).unwrap();
    gpu.set_debug_view(view);
    gpu.render(&overhead())
}

/// A display value as the linear light it encodes.
fn linear(value: u8) -> f64 {
    (f64::from(value) / 255.0).powf(2.2)
}

#[test]
fn albedo_comes_from_the_material_table() {
    for name in ["water", "marble", "blue light", "copper", "obsidian"] {
        let colour = MaterialTable::built_in().by_name(name).unwrap().colour;
        let image = of_floor(name, DebugView::Albedo);
        let encoded = colour.map(|c| f64::from(c).powf(1.0 / 2.2) * 255.0);
        assert!(
            all_pixels(&image).all(|p| (0..3).all(|c| (f64::from(p[c]) - encoded[c]).abs() <= 2.0)),
            "{name}"
        );
    }
}

#[test]
fn an_id_the_table_does_not_define_is_magenta() {
    let mut store = BrickStore::new();
    // 0 is a gap below the table's first id; 1000 and 65535 are past its end.
    let ids = [0, 1_000, 65_535];
    for (x, id) in ids.into_iter().enumerate() {
        store.set(VoxelCoord([x as i32, 0, 0]), Some(Material(id)));
    }
    let mut gpu = Offscreen::new(SIZE, SIZE, &Scene::new(&store, None)).unwrap();
    gpu.set_debug_view(DebugView::Albedo);
    for x in 0..ids.len() {
        let above = [x as f64 + 0.5, 3.0, 0.5];
        let image = gpu.render(&Camera::looking_at(above, [above[0], 0.0, 0.5]));
        assert_eq!(pixel(&image, 16, 16), [255, 0, 255], "x = {x}");
    }
}

#[test]
fn a_materials_voxels_differ_in_brightness_by_up_to_its_value_jitter() {
    // Rock has a value jitter of 0.1. The lighting is the same on the whole floor, so only the
    // variation tells Voxels apart.
    let rock = of_floor("rock", DebugView::Lit);
    let reds: Vec<f64> = all_pixels(&rock).map(|p| linear(p[0])).collect();
    let low = reds.iter().copied().fold(f64::MAX, f64::min);
    let high = reds.iter().copied().fold(f64::MIN, f64::max);
    assert!(high / low > 1.1, "Voxels barely differ: {low} to {high}");
    assert!(
        high / low < 1.23,
        "more than 10% either way: {low} to {high}"
    );
}

#[test]
fn a_material_without_jitter_is_one_flat_colour() {
    let glass = of_floor("clear glass", DebugView::Lit);
    assert!(all_pixels(&glass).all(|p| p == pixel(&glass, 0, 0)));
}

#[test]
fn the_variation_follows_the_voxel_not_the_view() {
    // Looking straight down from the height at which a pixel covers exactly one Voxel, with the
    // camera on a Voxel boundary, pixel i of a row sees Voxel i (at its centre). Moving the
    // camera 3 Voxels along x must then move every Voxel's colour 3 pixels along the row.
    let height = 16.0 / (70f64.to_radians() / 2.0).tan();
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor()).unwrap();
    let mut from = |x: f64| gpu.render(&Camera::looking_at([x, height, 16.0], [x, 0.0, 16.0]));
    let (a, b) = (from(16.0), from(19.0));

    let same = |p: [u8; 3], q: [u8; 3]| (0..3).all(|c| p[c].abs_diff(q[c]) <= 2);
    // Every whole-pixel shift of b against a that leaves all the pixels they share equal.
    let range = -4i32..=4;
    let shifts: Vec<(i32, i32)> = range
        .clone()
        .flat_map(|dx| range.clone().map(move |dy| (dx, dy)))
        .filter(|&(dx, dy)| {
            (0..SIZE as i32)
                .flat_map(|x| (0..SIZE as i32).map(move |y| (x, y)))
                .filter(|&(x, y)| {
                    (0..SIZE as i32).contains(&(x + dx)) && (0..SIZE as i32).contains(&(y + dy))
                })
                .all(|(x, y)| {
                    same(
                        pixel(&b, x as u32, y as u32),
                        pixel(&a, (x + dx) as u32, (y + dy) as u32),
                    )
                })
        })
        .collect();
    // Only the 3 Voxels the camera moved, one way or the other. A colour tied to the screen would
    // also match with no shift, and a flat colour at every shift.
    assert!(
        matches!(shifts[..], [(-3, 0)] | [(3, 0)]),
        "b matches a shifted by {shifts:?}"
    );

    // Neighbouring Voxels have different colours, so that matching means something.
    let differing = (0..SIZE - 1)
        .flat_map(|x| (0..SIZE).map(move |y| (x, y)))
        .filter(|&(x, y)| !same(pixel(&a, x, y), pixel(&a, x + 1, y)))
        .count();
    assert!(differing > (SIZE * (SIZE - 1) / 2) as usize, "{differing}");
}

#[test]
fn an_emissive_material_adds_its_light_to_the_lit_image() {
    // The sun is below the horizon, so the floor has only the ambient 0.25 of its colour; what
    // is above that is the emitted light.
    let lit_red = |name: &str| {
        let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material_id(name))).unwrap();
        gpu.set_sun(Sun::from_angles(0.0, -0.5, 0.04));
        linear(pixel(&gpu.render(&overhead()), 16, 16)[0])
    };
    let mushroom = MaterialTable::built_in()
        .by_name("glowing mushroom")
        .unwrap();
    let expected = f64::from(mushroom.emissive[0] * mushroom.emissive_intensity)
        + 0.25 * f64::from(mushroom.colour[0]);
    let got = lit_red("glowing mushroom");
    assert!((got - expected).abs() < 0.04, "{got} is not {expected}");
    // Moss is a plant too, but does not glow.
    assert!(lit_red("moss") < 0.1);
}

#[test]
fn a_materials_hue_varies_by_up_to_its_hue_jitter() {
    // How far the green-to-red ratio of the lit floor's pixels spreads. Rock has no hue jitter,
    // so its ratio is constant to within the rounding of 8-bit values; flowers turn up to 25
    // degrees either way.
    let spread = |name: &str| {
        let image = of_floor(name, DebugView::Lit);
        let ratios: Vec<f64> = all_pixels(&image)
            .map(|p| linear(p[1]) / linear(p[0]))
            .collect();
        let low = ratios.iter().copied().fold(f64::MAX, f64::min);
        let high = ratios.iter().copied().fold(f64::MIN, f64::max);
        high / low
    };
    assert!(spread("rock") < 1.1, "{}", spread("rock"));
    assert!(spread("flowers") > 1.5, "{}", spread("flowers"));
}

#[test]
fn the_history_length_view_of_the_reference_image_is_its_sample_count() {
    // The reference image has no upscaler: the history it has is its samples.
    let mut gpu = gpu(DebugView::HistoryLength);
    let one = gpu.render_reference(&overhead(), 1);
    near(pixel(&one, 3, 3), [25.5, 0.0, 76.5]);
    let sixteen = gpu.render_reference(&overhead(), 15);
    near(
        pixel(&sixteen, 3, 3),
        [255.0 * 0.46, 255.0 * 0.36, 255.0 * 0.26],
    );
}
