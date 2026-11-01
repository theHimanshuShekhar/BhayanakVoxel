//! THROWAWAY (#99, branch `measure/reference-99`, never merged): temporary shader variants of
//! the reference pass, to time one sample by part (bounces, shadow rays) and to time several
//! samples per pixel in one dispatch (k), plus a seed for independent truths.

use super::*;

/// A variant of the reference kernel. `Variant::PRODUCTION` is what `pathtrace.wgsl` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Variant {
    /// Samples per pixel in one dispatch, in an in-lane loop.
    pub k: u32,
    /// Bounces of indirect light after the camera ray's own hit (`MAX_BOUNCES`).
    pub bounces: u32,
    /// Whether the sun's shadow ray is cast at each hit.
    pub shadow: bool,
}

impl Variant {
    pub const PRODUCTION: Variant = Variant {
        k: 1,
        bounces: 4,
        shadow: true,
    };

    /// The name the variant's pass is timed under.
    pub fn name(&self) -> String {
        format!(
            "ref-k{}-b{}-{}",
            self.k,
            self.bounces,
            if self.shadow { "shadow" } else { "noshadow" }
        )
    }
}

/// The entry point of the variant: `k` samples per pixel in a loop, then the running mean.
const ENTRY: &str = r#"
@compute @workgroup_size(8, 8)
fn reference(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let first = frame.params.x;
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < K; i++) {
        var rng = seed_rng(id.xy, first + i, frame.params.y);
        let offset = vec2<f32>(next_f32(&rng), next_f32(&rng));
        sum += radiance(camera_ray(id.xy, size, offset), &rng);
    }
    let at = id.y * size.x + id.x;
    var mean = sum / f32(K);
    if first > 0u {
        mean = (accum[at].xyz * f32(first) + sum) / f32(first + K);
    }
    accum[at] = vec4<f32>(mean, 1.0);
    textureStore(output, id.xy, vec4<f32>(encode(mean), 1.0));
}
"#;

fn replace_once(source: &str, from: &str, to: &str) -> String {
    assert_eq!(
        source.matches(from).count(),
        1,
        "pathtrace.wgsl lost `{from}`"
    );
    source.replace(from, to)
}

/// The WGSL of `variant`: `pathtrace.wgsl` with its constants switched and its entry replaced.
pub(super) fn source(variant: Variant) -> String {
    let source = shaders::PATHTRACE;
    let source = replace_once(
        source,
        "const MAX_BOUNCES = 4u;",
        &format!(
            "const MAX_BOUNCES = {}u;\nconst SHADOW_RAYS = {};\nconst K = {}u;",
            variant.bounces, variant.shadow, variant.k
        ),
    );
    let source = replace_once(
        &source,
        "if facing > 0.0 && !trace(",
        "if SHADOW_RAYS && facing > 0.0 && !trace(",
    );
    let entry = source
        .find("@compute @workgroup_size(8, 8)")
        .expect("pathtrace.wgsl has its entry point");
    format!("{}{ENTRY}", &source[..entry])
}

impl Tracer {
    /// Sets the seed of the reference's random numbers (`RNG_SEED` by default). The image starts
    /// over.
    pub fn set_seed(&mut self, seed: u32) {
        self.seed = seed;
        self.accumulator.reset();
    }

    /// Records one dispatch of `variant`, adding `variant.k` samples per pixel to the running
    /// mean, the first of them sample `first_sample` (which must be 0 to start a new image).
    /// Timed by `profiler` as `variant.name()`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_variant(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
        variant: Variant,
        first_sample: u32,
    ) {
        if !self.variants.contains_key(&variant) {
            let pipeline = compute_pipeline(
                device,
                "reference variant",
                &source(variant),
                "reference",
                &self.camera_layout,
                &self.scene_layout,
            );
            self.variants.insert(variant, pipeline);
        }
        self.write_uniforms(queue, camera, sun, first_sample, CENTRE);
        self.dispatch(
            device,
            profiler,
            encoder,
            &variant.name(),
            &self.variants[&variant],
            target,
        );
    }
}

impl Target {
    /// The float running mean, one `vec4<f32>` a pixel, rows from the top.
    pub(crate) fn accum(&self) -> &wgpu::Buffer {
        &self.accum
    }
}
