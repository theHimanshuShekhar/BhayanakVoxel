//! Checks of the sampling maths in `shaders/sampling.wgsl`, run on the GPU through a small
//! kernel (there is no second implementation on the CPU to drift from it). They check the
//! properties that make the path tracer unbiased: that samples stay where they should, and that
//! they are distributed the way the estimator assumes.

use crate::{create_instance, read_buffer, request_gpu, shaders};

const PROBE: &str = r#"
@group(0) @binding(0) var<storage, read_write> out: array<vec4<f32>>;

const NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, -1.0),
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(-0.6, 0.0, 0.8),
    vec3<f32>(0.57735026, 0.57735026, 0.57735026),
);
const CONE_COS = 0.99;

// Four vec4s per thread: a cosine-weighted direction about a normal (w: the normal's index), a
// direction in the cone about that normal, the first two random numbers, and the first two of a
// stream seeded with the same values (to show seeding is deterministic).
@compute @workgroup_size(64)
fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= arrayLength(&out) / 4u {
        return;
    }
    let which = id.x % 6u;
    let n = NORMALS[which];
    var rng = seed_rng(vec2<u32>(id.x, 7u), id.x / 3u, 99u);
    let u = vec2<f32>(next_f32(&rng), next_f32(&rng));
    let v = vec2<f32>(next_f32(&rng), next_f32(&rng));
    out[id.x * 4u] = vec4<f32>(cosine_hemisphere(n, u), f32(which));
    out[id.x * 4u + 1u] = vec4<f32>(sample_cone(n, CONE_COS, v), 0.0);
    out[id.x * 4u + 2u] = vec4<f32>(u, v);
    var again = seed_rng(vec2<u32>(id.x, 7u), id.x / 3u, 99u);
    out[id.x * 4u + 3u] = vec4<f32>(next_f32(&again), next_f32(&again), next_f32(&again), next_f32(&again));
}
"#;

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

/// Runs the probe: for each thread, four vec4s of output (see the kernel).
fn run_probe() -> Vec<[f32; 4]> {
    let (_adapter, device, queue) = request_gpu(&create_instance(), None, true).unwrap();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sampling probe"),
        source: wgpu::ShaderSource::Wgsl(format!("{}\n{PROBE}", shaders::SAMPLING).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sampling probe"),
        layout: None,
        module: &module,
        entry_point: Some("probe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let size = (THREADS * 4 * 16) as u64;
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
        pass.dispatch_workgroups((THREADS as u32).div_ceil(64), 1, 1);
    }
    let bytes = read_buffer(&device, &queue, encoder, &buffer, size);
    bytemuck::cast_slice::<u8, [f32; 4]>(&bytes).to_vec()
}

fn xyz(v: [f32; 4]) -> [f32; 3] {
    [v[0], v[1], v[2]]
}

#[test]
fn cosine_samples_are_unit_vectors_in_the_hemisphere_with_the_cosine_distribution() {
    let out = run_probe();
    let mut cos_sum = [0.0f64; 6];
    let mut count = [0u32; 6];
    for t in 0..THREADS {
        let [x, y, z, which] = out[t * 4];
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
        let d = xyz(out[t * 4 + 1]);
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
    let module_src = format!(
        "{}\n@group(0) @binding(0) var<storage, read_write> out: array<vec4<f32>>;\n\
         @compute @workgroup_size(1)\nfn probe() {{\n\
         out[0] = vec4<f32>(sample_cone(vec3<f32>(0.0, 1.0, 0.0), 1.0, vec2<f32>(0.3, 0.9)), 0.0);\n}}",
        shaders::SAMPLING
    );
    let (_adapter, device, queue) = request_gpu(&create_instance(), None, true).unwrap();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(module_src.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("probe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
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
        pass.dispatch_workgroups(1, 1, 1);
    }
    let bytes = read_buffer(&device, &queue, encoder, &buffer, 16);
    let d: &[f32] = bytemuck::cast_slice(&bytes);
    assert!(
        d[0].abs() < 1e-3 && (d[1] - 1.0).abs() < 1e-5 && d[2].abs() < 1e-3,
        "{d:?}"
    );
}

#[test]
fn random_numbers_are_uniform_in_zero_to_one_and_depend_only_on_the_seed() {
    let out = run_probe();
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut n = 0.0f64;
    for t in 0..THREADS {
        let drawn = out[t * 4 + 2];
        // Same seed, same stream.
        assert_eq!(drawn, out[t * 4 + 3], "thread {t}");
        for x in drawn {
            assert!((0.0..1.0).contains(&x), "{x}");
            sum += f64::from(x);
            sum_sq += f64::from(x) * f64::from(x);
            n += 1.0;
        }
    }
    let mean = sum / n;
    let variance = sum_sq / n - mean * mean;
    assert!((mean - 0.5).abs() < 0.01, "mean {mean}");
    assert!((variance - 1.0 / 12.0).abs() < 0.005, "variance {variance}");
    // Neighbouring pixels and frames get different streams.
    assert_ne!(out[2], out[6]);
}
