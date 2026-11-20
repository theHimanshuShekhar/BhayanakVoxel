//! Checks of the sampling maths in `shaders/sampling.wgsl`, run on the GPU through small kernels
//! (the CPU has only a port of the integer sampler, to compare bits with). They check the
//! properties that make the path tracer unbiased: that samples stay where they should, and that
//! they are distributed the way the estimator assumes. The sample sequence's own tests are at the
//! end (#188, ADR 0005, `docs/research/reference-sampling.md` "How to verify").

use crate::{read_buffer, request_gpu, shaders, shared_instance};

/// Runs `body` (WGSL after `sampling.wgsl`, with `out: array<u32>` declared before it) in
/// `threads` threads of a kernel named `probe` that takes a `@builtin(global_invocation_id) id`,
/// and returns the `out_words` words it wrote.
fn run(body: &str, threads: usize, out_words: usize) -> Vec<u32> {
    let (_adapter, device, queue) = request_gpu(shared_instance(), None, true).unwrap();
    let source = format!(
        "{}\n@group(0) @binding(0) var<storage, read_write> out: array<u32>;\n{body}",
        shaders::SAMPLING
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sampling probe"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sampling probe"),
        layout: None,
        module: &module,
        entry_point: Some("probe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let size = (out_words * 4) as u64;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sampling probe out"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        // At most 65535 workgroups in a dimension: spread them over y.
        let groups = (threads as u32).div_ceil(64);
        pass.dispatch_workgroups(groups.min(32768), groups.div_ceil(32768), 1);
    }
    let bytes = read_buffer(&device, &queue, encoder, &buffer, size);
    bytemuck::cast_slice::<u8, u32>(&bytes).to_vec()
}

/// The thread's index, whatever the shape the dispatch was spread over.
const THREAD: &str = "let t = id.x + id.y * 32768u * 64u;";

fn floats(words: &[u32]) -> Vec<f32> {
    words.iter().map(|&w| f32::from_bits(w)).collect()
}

const THREADS: usize = 6 * 4096;
const CONE_COS: f32 = 0.99;
const NORMALS: [[f32; 3]; 6] = [
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
    [1.0, 0.0, 0.0],
    [-0.6, 0.0, 0.8],
    [0.577_350_26, 0.577_350_26, 0.577_350_26],
];

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

/// Per thread, 8 words: a cosine-weighted direction about a normal and the normal's index, a
/// direction in the cone about that normal and a pad, as floats.
fn run_probe() -> Vec<[f32; 4]> {
    let body = r#"
const NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, -1.0),
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(-0.6, 0.0, 0.8),
    vec3<f32>(0.57735026, 0.57735026, 0.57735026),
);
const CONE_COS = 0.99;

@compute @workgroup_size(64)
fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
    THREAD
    if t >= 24576u {
        return;
    }
    let which = t % 6u;
    let n = NORMALS[which];
    let pixel_seed = pixel_hash(vec2<u32>(t, 7u), 99u);
    let u = vec2<f32>(hashed_f32(pixel_seed, t / 3u, 0u), hashed_f32(pixel_seed, t / 3u, 1u));
    let v = vec2<f32>(hashed_f32(pixel_seed, t / 3u, 2u), hashed_f32(pixel_seed, t / 3u, 3u));
    let d = cosine_hemisphere(n, u);
    let c = sample_cone(n, CONE_COS, v);
    out[t * 8u] = bitcast<u32>(d.x);
    out[t * 8u + 1u] = bitcast<u32>(d.y);
    out[t * 8u + 2u] = bitcast<u32>(d.z);
    out[t * 8u + 3u] = bitcast<u32>(f32(which));
    out[t * 8u + 4u] = bitcast<u32>(c.x);
    out[t * 8u + 5u] = bitcast<u32>(c.y);
    out[t * 8u + 6u] = bitcast<u32>(c.z);
}
"#
    .replace("THREAD", THREAD);
    floats(&run(&body, THREADS, THREADS * 8))
        .as_chunks::<4>()
        .0
        .to_vec()
}

#[test]
fn cosine_samples_are_unit_vectors_in_the_hemisphere_with_the_cosine_distribution() {
    let out = run_probe();
    let mut cos_sum = [0.0f64; 6];
    let mut count = [0u32; 6];
    for t in 0..THREADS {
        let [x, y, z, which] = out[t * 2];
        let which = which as usize;
        let d = [x, y, z];
        assert!((dot(d, d) - 1.0).abs() < 1e-4, "not a unit vector: {d:?}");
        let cos = dot(d, NORMALS[which]);
        assert!(
            cos >= -1e-5,
            "below the surface: {d:?} about {:?}",
            NORMALS[which]
        );
        cos_sum[which] += f64::from(cos);
        count[which] += 1;
    }
    // For a density proportional to cos(theta), the mean of cos(theta) is 2/3. A uniform
    // hemisphere would give 1/2.
    for which in 0..6 {
        let mean = cos_sum[which] / f64::from(count[which]);
        assert!(
            (mean - 2.0 / 3.0).abs() < 0.01,
            "normal {which}: mean cosine {mean}"
        );
    }
}

#[test]
fn cone_samples_are_unit_vectors_inside_the_cone_and_uniform_over_it() {
    let out = run_probe();
    let mut cos_sum = 0.0f64;
    for t in 0..THREADS {
        let [x, y, z, _] = out[t * 2 + 1];
        let d = [x, y, z];
        assert!((dot(d, d) - 1.0).abs() < 1e-4, "not a unit vector: {d:?}");
        let cos = dot(d, NORMALS[t % 6]);
        assert!(cos >= CONE_COS - 1e-4, "outside the cone: cos {cos}");
        cos_sum += f64::from(cos);
    }
    // Uniform over the cap's solid angle, cos(theta) is uniform between CONE_COS and 1.
    let mean = cos_sum / THREADS as f64;
    let expected = f64::from(1.0 + CONE_COS) / 2.0;
    assert!(
        (mean - expected).abs() < 1e-4,
        "mean cosine {mean}, expected {expected}"
    );
}

#[test]
fn a_zero_radius_cone_is_exactly_the_axis() {
    // Hard shadows: with cos(radius) = 1 every sample is the axis itself.
    let out = floats(&run(
        "@compute @workgroup_size(1)\nfn probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
         let d = sample_cone(vec3<f32>(0.0, 1.0, 0.0), 1.0, vec2<f32>(0.3, 0.9));\n\
         out[0] = bitcast<u32>(d.x); out[1] = bitcast<u32>(d.y); out[2] = bitcast<u32>(d.z);\n}",
        1,
        4,
    ));
    assert!(
        out[0].abs() < 1e-3 && (out[1] - 1.0).abs() < 1e-5 && out[2].abs() < 1e-3,
        "{out:?}"
    );
}

/// The solid angle of the rectangle `[x0, x1] x [y0, y1]` on the plane at z = `z`, seen from the
/// origin, by midpoint integration of `z / r^3` over it (the CPU's second writing of what
/// `sph_quad` computes with spherical trigonometry).
fn rectangle_solid_angle(x0: f64, x1: f64, y0: f64, y1: f64, z: f64) -> f64 {
    let n = 400;
    let (dx, dy) = ((x1 - x0) / n as f64, (y1 - y0) / n as f64);
    let mut sum = 0.0;
    for i in 0..n {
        for j in 0..n {
            let (x, y) = (x0 + (i as f64 + 0.5) * dx, y0 + (j as f64 + 0.5) * dy);
            sum += z.abs() / (x * x + y * y + z * z).powf(1.5) * dx * dy;
        }
    }
    sum
}

#[test]
fn rectangle_samples_stay_on_the_rectangle_and_are_uniform_over_its_solid_angle() {
    // (`z0` is negative, the rectangle behind the origin's z, as the shader's callers give it.)
    // A rectangle at z = -2 that the origin is not over the middle of, and one it is just under a
    // corner of: both are what an emitter's face looks like from a shaded voxel.
    let cases = [
        (-1.0f32, 2.0f32, -0.5f32, 1.5f32, -2.0f32),
        (0.0, 3.0, 0.0, 0.5, -0.75),
    ];
    for (x0, x1, y0, y1, z0) in cases {
        let body = r#"
@compute @workgroup_size(64)
fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
    THREAD
    if t >= 16384u {
        return;
    }
    let q = sph_quad(X0, Y0, Z0, X1, Y1);
    let pixel_seed = pixel_hash(vec2<u32>(t, 3u), 5u);
    let u = vec2<f32>(hashed_f32(pixel_seed, t, 0u), hashed_f32(pixel_seed, t, 1u));
    let p = sph_quad_sample(q, u);
    out[t * 4u] = bitcast<u32>(p.x);
    out[t * 4u + 1u] = bitcast<u32>(p.y);
    out[t * 4u + 2u] = bitcast<u32>(q.solid_angle);
}
"#
        .replace("THREAD", THREAD)
        .replace("X0", &format!("{x0:?}"))
        .replace("X1", &format!("{x1:?}"))
        .replace("Y0", &format!("{y0:?}"))
        .replace("Y1", &format!("{y1:?}"))
        .replace("Z0", &format!("{z0:?}"));
        let out = floats(&run(&body, 16384, 16384 * 4));
        let expected = rectangle_solid_angle(
            f64::from(x0),
            f64::from(x1),
            f64::from(y0),
            f64::from(y1),
            f64::from(z0),
        );
        let solid_angle = f64::from(out[2]);
        assert!(
            (solid_angle - expected).abs() < 2e-3 * expected,
            "solid angle {solid_angle}, integrated {expected}"
        );
        // Uniform over the solid angle: the share of samples left of a line is that part's share.
        let split = x0 + (x1 - x0) * 0.3;
        let share = rectangle_solid_angle(
            f64::from(x0),
            f64::from(split),
            f64::from(y0),
            f64::from(y1),
            f64::from(z0),
        ) / expected;
        let mut left = 0usize;
        for t in 0..16384 {
            let (x, y) = (out[t * 4], out[t * 4 + 1]);
            assert!(
                x >= x0 - 1e-3 && x <= x1 + 1e-3 && y >= y0 - 1e-3 && y <= y1 + 1e-3,
                "({x}, {y}) is off the rectangle"
            );
            left += usize::from(x < split);
        }
        let measured = left as f64 / 16384.0;
        assert!(
            (measured - share).abs() < 0.02,
            "{measured} of the samples are left of the line; the solid angle says {share}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The sample sequence.
//
// The probes call `sample_group` and `sobol4` from `sampling.wgsl`, which is what the Reference
// view's kernel compiles. The CPU port below is a second, independent writing of the integer
// maths, from the paper and from Joe and Kuo's recurrence, and not a copy of the shader.

/// A CPU port of the sampler, and of the direction numbers' recurrence.
mod cpu {
    pub fn pcg(v: u32) -> u32 {
        let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
        (word >> 22) ^ word
    }

    pub fn pixel_hash(pixel: [u32; 2], seed: u32) -> u32 {
        pcg(pixel[0].wrapping_add(pcg(pixel[1].wrapping_add(pcg(seed)))))
    }

    fn unit(bits: u32) -> f32 {
        (bits >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    pub fn hashed(pixel_seed: u32, sample: u32, dim: u32) -> f32 {
        unit(pcg(
            pcg(pixel_seed ^ pcg(dim.wrapping_add(0x9E37_79B9))).wrapping_add(sample)
        ))
    }

    fn hash_combine(seed: u32, i: u32) -> u32 {
        pcg(seed ^ i.wrapping_mul(0x9E37_79B9))
    }

    fn laine_karras(x: u32, seed: u32) -> u32 {
        let mut v = x.wrapping_add(seed);
        v ^= v.wrapping_mul(0x6c50_b47c);
        v ^= v.wrapping_mul(0xb82f_1e52);
        v ^= v.wrapping_mul(0xc7af_e638);
        v ^= v.wrapping_mul(0x8d22_f6e6);
        v
    }

    pub fn nested_uniform_scramble(x: u32, seed: u32) -> u32 {
        laine_karras(x.reverse_bits(), seed).reverse_bits()
    }

    /// Direction numbers of Sobol dimension `d` (0 to 3), 32 of them, from Joe and Kuo's
    /// `new-joe-kuo-6.21201`: (degree s, coefficients a, initial m) for dimensions 2 to 4.
    pub fn directions(d: usize) -> [u32; 32] {
        const JOE_KUO: [(usize, u32, [u32; 3]); 3] =
            [(1, 0, [1, 0, 0]), (2, 1, [1, 3, 0]), (3, 1, [1, 3, 1])];
        let mut v = [0u32; 32];
        if d == 0 {
            for (k, v) in v.iter_mut().enumerate() {
                *v = 1 << (31 - k);
            }
            return v;
        }
        let (s, a, m) = JOE_KUO[d - 1];
        for k in 0..s {
            v[k] = m[k] << (31 - k);
        }
        for k in s..32 {
            let mut x = v[k - s] ^ (v[k - s] >> s);
            for i in 1..s {
                if (a >> (s - 1 - i)) & 1 == 1 {
                    x ^= v[k - i];
                }
            }
            v[k] = x;
        }
        v
    }

    pub fn sobol4(index: u32) -> [u32; 4] {
        let tables: Vec<[u32; 32]> = (0..4).map(directions).collect();
        let mut point = [0u32; 4];
        for k in 0..32 {
            if (index >> k) & 1 == 1 {
                for (p, table) in point.iter_mut().zip(&tables) {
                    *p ^= table[k];
                }
            }
        }
        point
    }

    pub fn group(pixel_seed: u32, sample: u32, group: u32, white: bool) -> [f32; 4] {
        if white || group >= 18 {
            return std::array::from_fn(|i| hashed(pixel_seed, sample, group * 4 + i as u32));
        }
        let seed = hash_combine(pixel_seed, group);
        let point = sobol4(nested_uniform_scramble(sample, seed));
        std::array::from_fn(|i| {
            unit(nested_uniform_scramble(
                point[i],
                hash_combine(seed, i as u32),
            ))
        })
    }
}

/// What a sample group probe is asked: `(pixel, seed, sample, group, white)` for each thread.
type Query = ([u32; 2], u32, u32, u32, bool);

/// `sample_group` on the GPU for each query: four floats (as bits) each.
fn groups_on_gpu(queries: &[Query]) -> Vec<[u32; 4]> {
    // The queries go in as constants, a table of 8 words a thread.
    let table: Vec<String> = queries
        .iter()
        .map(|&(pixel, seed, sample, group, white)| {
            format!(
                "{}u,{}u,{seed}u,{sample}u,{group}u,{}u",
                pixel[0],
                pixel[1],
                u32::from(white)
            )
        })
        .collect();
    let body = format!(
        "const QUERIES = array<u32, {}>({});\n\
         @compute @workgroup_size(64)\nfn probe(@builtin(global_invocation_id) id: vec3<u32>) {{\n\
         {THREAD}\n if t >= {}u {{ return; }}\n let q = t * 6u;\n\
         let pixel_seed = pixel_hash(vec2<u32>(QUERIES[q], QUERIES[q + 1u]), QUERIES[q + 2u]);\n\
         let g = sample_group(pixel_seed, QUERIES[q + 3u], QUERIES[q + 4u], QUERIES[q + 5u] != 0u);\n\
         out[t * 4u] = bitcast<u32>(g.x); out[t * 4u + 1u] = bitcast<u32>(g.y);\n\
         out[t * 4u + 2u] = bitcast<u32>(g.z); out[t * 4u + 3u] = bitcast<u32>(g.w);\n}}",
        queries.len() * 6,
        table.join(","),
        queries.len()
    );
    run(&body, queries.len(), queries.len() * 4)
        .as_chunks::<4>()
        .0
        .to_vec()
}

/// A kernel that loops over a grid instead of reading a table, for the tests that need many
/// points: each thread is one pixel `(t % 64, t / 64)` of a 64 by 64 image (4096 threads) and
/// writes `samples` samples of `group` (its first `slots` slots) in a row.
fn grid_on_gpu(group: u32, samples: u32, first_sample: u32, seed: u32, white: bool) -> Vec<f32> {
    grid_of(
        samples,
        seed,
        &format!("sample_group(pixel_seed, {first_sample}u + s, {group}u, {white})"),
    )
}

/// `grid_on_gpu` for any WGSL expression of `pixel_seed` and the sample's number `s` that makes a
/// `vec4<f32>`.
fn grid_of(samples: u32, seed: u32, expression: &str) -> Vec<f32> {
    let body = format!(
        "@compute @workgroup_size(64)\nfn probe(@builtin(global_invocation_id) id: vec3<u32>) {{\n\
         {THREAD}\n if t >= 4096u {{ return; }}\n\
         let pixel_seed = pixel_hash(vec2<u32>(t % 64u, t / 64u), {seed}u);\n\
         for (var s = 0u; s < {samples}u; s++) {{\n\
           let g = {expression};\n\
           let at = (t * {samples}u + s) * 4u;\n\
           out[at] = bitcast<u32>(g.x); out[at + 1u] = bitcast<u32>(g.y);\n\
           out[at + 2u] = bitcast<u32>(g.z); out[at + 3u] = bitcast<u32>(g.w);\n }}\n}}",
    );
    floats(&run(&body, 4096, 4096 * samples as usize * 4))
}

/// Group `group` as `sample_group` makes it, but with the index shuffle left out: every group
/// reads the same Sobol index and only the points are scrambled (Burley's section 4 says why
/// that is not enough). The fault that the per-pixel correlation test must be able to see.
fn without_the_index_shuffle(group: u32) -> String {
    format!(
        "vec4<f32>(\
         unit_f32(nested_uniform_scramble(sobol4(s).x, hash_combine(hash_combine(pixel_seed, {group}u), 0u))), \
         unit_f32(nested_uniform_scramble(sobol4(s).y, hash_combine(hash_combine(pixel_seed, {group}u), 1u))), \
         unit_f32(nested_uniform_scramble(sobol4(s).z, hash_combine(hash_combine(pixel_seed, {group}u), 2u))), \
         unit_f32(nested_uniform_scramble(sobol4(s).w, hash_combine(hash_combine(pixel_seed, {group}u), 3u))))"
    )
}

#[test]
fn the_first_16_sobol_points_are_the_published_values() {
    // Sobol's sequence in the first four dimensions, in the order of the index, from Joe and
    // Kuo's direction numbers (`new-joe-kuo-6.21201`): the first two columns are the ones in
    // Burley's paper (section 2.2). The same points, as a set, are what scipy's unscrambled
    // `qmc.Sobol(d=4)` gives, in Gray-code order.
    #[rustfmt::skip]
    const PUBLISHED: [[f64; 4]; 16] = [
        [0.0, 0.0, 0.0, 0.0],
        [0.5, 0.5, 0.5, 0.5],
        [0.25, 0.75, 0.75, 0.75],
        [0.75, 0.25, 0.25, 0.25],
        [0.125, 0.625, 0.375, 0.125],
        [0.625, 0.125, 0.875, 0.625],
        [0.375, 0.375, 0.625, 0.875],
        [0.875, 0.875, 0.125, 0.375],
        [0.0625, 0.9375, 0.5625, 0.3125],
        [0.5625, 0.4375, 0.0625, 0.8125],
        [0.3125, 0.1875, 0.3125, 0.5625],
        [0.8125, 0.6875, 0.8125, 0.0625],
        [0.1875, 0.3125, 0.9375, 0.4375],
        [0.6875, 0.8125, 0.4375, 0.9375],
        [0.4375, 0.5625, 0.1875, 0.6875],
        [0.9375, 0.0625, 0.6875, 0.1875],
    ];
    let body = "@compute @workgroup_size(64)\nfn probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
         if id.x >= 16u { return; }\n let p = sobol4(id.x);\n\
         out[id.x * 4u] = p.x; out[id.x * 4u + 1u] = p.y; out[id.x * 4u + 2u] = p.z; out[id.x * 4u + 3u] = p.w;\n}";
    let got = run(body, 16, 64);
    for (i, row) in PUBLISHED.iter().enumerate() {
        for d in 0..4 {
            let value = f64::from(got[i * 4 + d]) / 4_294_967_296.0;
            assert_eq!(value, row[d], "point {i}, dimension {d}");
        }
    }
}

#[test]
fn the_shaders_direction_numbers_are_joe_and_kuos_all_32_of_each() {
    // sobol4(1 << k) is the k-th direction number of every dimension.
    let body = "@compute @workgroup_size(32)\nfn probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
         let p = sobol4(1u << id.x);\n\
         out[id.x * 4u] = p.x; out[id.x * 4u + 1u] = p.y; out[id.x * 4u + 2u] = p.z; out[id.x * 4u + 3u] = p.w;\n}";
    let got = run(body, 32, 128);
    for d in 0..4 {
        let want = cpu::directions(d);
        for k in 0..32 {
            assert_eq!(
                got[k * 4 + d],
                want[k],
                "dimension {d}, direction number {k}"
            );
        }
    }
}

#[test]
fn the_sampler_gives_the_same_bits_as_the_cpu_port_and_on_a_second_run() {
    // Whole numbers only: the bits are the same on every device. Both runs here are the same
    // driver, so what this holds is that the shader and the port, written apart, agree to the
    // bit, which no float rounding could do; and that a second run repeats the first.
    let mut queries: Vec<Query> = Vec::new();
    for (i, &sample) in [0u32, 1, 2, 3, 63, 64, 1000, 8191, 8192, 1 << 20, u32::MAX]
        .iter()
        .enumerate()
    {
        for group in [0u32, 1, 2, 5, 10, 15, 16, 17, 100] {
            for white in [false, true] {
                queries.push((
                    [i as u32 * 37, 1439 - i as u32],
                    0xB4A7_A9AC ^ (i as u32),
                    sample,
                    group,
                    white,
                ));
            }
        }
    }
    let first = groups_on_gpu(&queries);
    assert_eq!(first, groups_on_gpu(&queries), "a second run differs");
    for (q, got) in queries.iter().zip(&first) {
        let &(pixel, seed, sample, group, white) = q;
        let want = cpu::group(cpu::pixel_hash(pixel, seed), sample, group, white);
        let want: [u32; 4] = want.map(f32::to_bits);
        assert_eq!(
            *got, want,
            "pixel {pixel:?} seed {seed} sample {sample} group {group} white {white}"
        );
    }
}

#[test]
fn the_same_pixel_sample_and_seed_give_the_same_values_and_another_seed_does_not() {
    let q = |seed| vec![([5, 9], seed, 3, 2, false)];
    assert_eq!(groups_on_gpu(&q(1)), groups_on_gpu(&q(1)));
    assert_ne!(groups_on_gpu(&q(1)), groups_on_gpu(&q(2)));
    // And the pixel and the sample matter.
    assert_ne!(
        groups_on_gpu(&[([5, 9], 1, 3, 2, false)]),
        groups_on_gpu(&[([6, 9], 1, 3, 2, false)])
    );
    assert_ne!(
        groups_on_gpu(&[([5, 9], 1, 3, 2, false)]),
        groups_on_gpu(&[([5, 9], 1, 4, 2, false)])
    );
}

#[test]
fn every_dimension_is_uniform_in_zero_to_one() {
    // Dimensions 0 to 71: the 64 of Sobol and eight hashed. Sixty-four samples of each of 4096
    // pixels, in both the Sobol and the white mode, at the first samples and later ones.
    for white in [false, true] {
        for group in 0..18 {
            for first_sample in [0, 64, 1000] {
                let values = grid_on_gpu(group, 64, first_sample, 7, white);
                for slot in 0..4 {
                    let column: Vec<f64> = values
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f64::from(c[slot]))
                        .collect();
                    let n = column.len() as f64;
                    let mean = column.iter().sum::<f64>() / n;
                    let variance = column.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
                    let what = format!(
                        "dimension {}, white {white}, from sample {first_sample}",
                        group * 4 + slot as u32
                    );
                    assert!(column.iter().all(|v| (0.0..1.0).contains(v)), "{what}");
                    assert!((mean - 0.5).abs() < 0.003, "{what}: mean {mean}");
                    assert!(
                        (variance - 1.0 / 12.0).abs() < 0.002,
                        "{what}: variance {variance}"
                    );
                    // Deciles: every tenth of [0, 1) holds a tenth of the values.
                    let mut bins = [0u32; 10];
                    for v in &column {
                        bins[(v * 10.0) as usize] += 1;
                    }
                    for (b, &count) in bins.iter().enumerate() {
                        let share = f64::from(count) / n;
                        assert!((share - 0.1).abs() < 0.005, "{what}: bin {b} holds {share}");
                    }
                }
            }
        }
    }
}

/// Whether the points (x, y) put exactly one point in each cell of every grid of `2^i` by
/// `2^(k-i)` cells, for `i` in 0..=k, where there are `2^k` of them: a (0, k, 2)-net.
fn is_a_net(points: &[(f32, f32)], k: u32) -> Result<(), String> {
    assert_eq!(points.len(), 1 << k);
    for i in 0..=k {
        let (nx, ny) = (1usize << i, 1usize << (k - i));
        let mut cells = vec![0u32; nx * ny];
        for &(x, y) in points {
            cells[(x * nx as f32) as usize * ny + (y * ny as f32) as usize] += 1;
        }
        if let Some(bad) = cells.iter().position(|&c| c != 1) {
            return Err(format!(
                "the {nx} by {ny} grid has {} points in cell {bad}",
                cells[bad]
            ));
        }
    }
    Ok(())
}

#[test]
fn the_first_pair_is_stratified_64_samples_put_one_point_in_each_cell_of_an_8_by_8_grid() {
    // Every group's first pair, in a sample of the 4096 pixels: the 64 samples of a pixel are a
    // (0, 6, 2)-net, which holds one in each cell of the 8 by 8 grid and of the 4 by 16 and the
    // 64 by 1 and every other. And so are the 16 samples from sample 16, and those from 192. Only
    // the first pair of a group is held to be a (0, m, 2)-net: Sobol's third and fourth
    // dimensions are not, so the sun's disc (slots 2 and 3) is stratified less well.
    for group in 0..18 {
        for (first_sample, count) in [(0u32, 64u32), (64, 64), (16, 16), (192, 64)] {
            let values = grid_on_gpu(group, count, first_sample, 3, false);
            for pixel in (0..4096).step_by(37) {
                {
                    let slots = (0, 1);
                    let points: Vec<(f32, f32)> = (0..count as usize)
                        .map(|s| {
                            let at = (pixel * count as usize + s) * 4;
                            (values[at + slots.0], values[at + slots.1])
                        })
                        .collect();
                    is_a_net(&points, count.trailing_zeros()).unwrap_or_else(|e| {
                        panic!("group {group}, slots {slots:?}, pixel {pixel}, from sample {first_sample}: {e}")
                    });
                }
            }
        }
    }
}

#[test]
fn the_net_check_fails_on_white_noise() {
    // A check that cannot fail is not a check: the same test on white noise must find a cell
    // that is empty (64 random points leave one in 8 by 8 cells empty with certainty near 1).
    let values = grid_on_gpu(0, 64, 0, 3, true);
    let failures = (0..4096)
        .filter(|pixel| {
            let points: Vec<(f32, f32)> = (0..64)
                .map(|s| {
                    (
                        values[(pixel * 64 + s) * 4],
                        values[(pixel * 64 + s) * 4 + 1],
                    )
                })
                .collect();
            is_a_net(&points, 6).is_err()
        })
        .count();
    assert_eq!(failures, 4096);
}

/// The correlation coefficient of two columns.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (
        a.iter().map(|&v| f64::from(v)).sum::<f64>() / n,
        b.iter().map(|&v| f64::from(v)).sum::<f64>() / n,
    );
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (&a, &b) in a.iter().zip(b) {
        let (da, db) = (f64::from(a) - ma, f64::from(b) - mb);
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    cov / (va * vb).sqrt()
}

#[test]
fn groups_and_neighbouring_pixels_are_not_correlated() {
    // 4096 pixels of 16 samples each: 65 536 pairs, so a correlation of about 0.004 is one
    // standard error, and 0.02 is five of them. Padding with a scramble for each group is what
    // keeps one group from following another (Burley, section 4). The samples of one pixel are
    // not asked to be uncorrelated with each other: consecutive ones are anti-correlated, which is
    // stratification (the next sample falls in the other half), and why the error falls faster.
    const SAMPLES: usize = 16;
    let columns = |group, seed| {
        let values = grid_on_gpu(group, SAMPLES as u32, 0, seed, false);
        (0..4)
            .map(|slot| {
                values
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| c[slot])
                    .collect::<Vec<f32>>()
            })
            .collect::<Vec<_>>()
    };
    let g0 = columns(0, 11);
    let g1 = columns(1, 11);
    let g2 = columns(2, 11);
    let g9 = columns(9, 11);
    let g20 = columns(20, 11);
    let limit = 0.02;
    // Between groups, slot against the same and another slot, near and far, hashed too.
    for (name, a, b) in [
        ("0.x and 1.x", &g0[0], &g1[0]),
        ("0.x and 1.y", &g0[0], &g1[1]),
        ("0.y and 1.x", &g0[1], &g1[0]),
        ("0.z and 1.z", &g0[2], &g1[2]),
        ("1.x and 2.x", &g1[0], &g2[0]),
        ("0.x and 9.x", &g0[0], &g9[0]),
        ("0.w and 9.w", &g0[3], &g9[3]),
        ("9.x and 20.x (hashed)", &g9[0], &g20[0]),
    ] {
        let r = correlation(a, b);
        assert!(r.abs() < limit, "{name}: correlation {r}");
    }
    // Within a group.
    for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3)] {
        let r = correlation(&g0[a], &g0[b]);
        assert!(r.abs() < limit, "slots {a} and {b}: correlation {r}");
    }
    // Between neighbouring pixels: (x, y) with (x + 1, y) and with (x, y + 1), the same sample.
    for (slot, column) in g0.iter().enumerate() {
        let (mut a, mut right, mut b, mut down) = (vec![], vec![], vec![], vec![]);
        for y in 0..63 {
            for x in 0..63 {
                for s in 0..SAMPLES {
                    let at = |x: usize, y: usize| column[(y * 64 + x) * SAMPLES + s];
                    a.push(at(x, y));
                    right.push(at(x + 1, y));
                    b.push(at(x, y));
                    down.push(at(x, y + 1));
                }
            }
        }
        for (name, one, other) in [("right", &a, &right), ("down", &b, &down)] {
            let r = correlation(one, other);
            assert!(
                r.abs() < limit,
                "slot {slot}, neighbour {name}: correlation {r}"
            );
        }
    }
}

/// The mean over the pixels of |r|, the correlation of the pixel's own `samples` samples of `a`
/// with those of `b`. Each pixel has its own scramble, so the sign of a pixel's r is random and
/// the pooled correlation hides a dependence between the groups; |r| does not.
fn mean_absolute_correlation_per_pixel(a: &[f32], b: &[f32], samples: usize) -> f64 {
    let pixels = a.len() / samples;
    (0..pixels)
        .map(|p| correlation(&a[p * samples..][..samples], &b[p * samples..][..samples]).abs())
        .sum::<f64>()
        / pixels as f64
}

fn column(values: &[f32], slot: usize) -> Vec<f32> {
    values.as_chunks::<4>().0.iter().map(|c| c[slot]).collect()
}

#[test]
fn the_groups_of_one_pixel_are_not_correlated_with_each_other() {
    // The 64 samples of one pixel, group 0 against group 1 (and 9): independent numbers give a
    // mean |r| of about 0.099 (white noise through this code), and the sampler gives 0.125: the
    // index shuffle keeps the samples in blocks (indices 2j and 2j + 1 move together), so there
    // are effectively 32 pairs and not 64 independent samples. A group that reads the same index as another, and
    // is only scrambled differently, gives 0.7 or more, though the pooled correlation over the
    // pixels is no different from zero (see the next test).
    const SAMPLES: usize = 64;
    let g = |group| grid_on_gpu(group, SAMPLES as u32, 0, 21, false);
    let (g0, g1, g9) = (g(0), g(1), g(9));
    for (name, a, b) in [
        ("0.x and 1.x", column(&g0, 0), column(&g1, 0)),
        ("0.y and 1.y", column(&g0, 1), column(&g1, 1)),
        ("0.x and 9.x", column(&g0, 0), column(&g9, 0)),
        ("0.z and 1.z", column(&g0, 2), column(&g1, 2)),
    ] {
        let r = mean_absolute_correlation_per_pixel(&a, &b, SAMPLES);
        eprintln!("{name}: mean |r| per pixel {r:.3}");
        assert!(r < 0.15, "{name}: mean |r| per pixel {r}");
    }
}

#[test]
fn the_per_pixel_correlation_check_fails_without_the_index_shuffle() {
    // The same check on groups that read one index: it must fail, and the pooled correlation of
    // the same numbers must not, which is why the check above is per pixel.
    const SAMPLES: usize = 64;
    let g0 = grid_of(SAMPLES as u32, 21, &without_the_index_shuffle(0));
    let g1 = grid_of(SAMPLES as u32, 21, &without_the_index_shuffle(1));
    let r = mean_absolute_correlation_per_pixel(&column(&g0, 0), &column(&g1, 0), SAMPLES);
    let pooled = correlation(&column(&g0, 0), &column(&g1, 0));
    eprintln!("no shuffle: mean |r| per pixel {r:.3}, pooled {pooled:.4}");
    assert!(r > 0.4, "the fault was not seen: mean |r| per pixel {r}");
    assert!(
        pooled.abs() < 0.02,
        "the pooled correlation sees it too: {pooled}"
    );
}
