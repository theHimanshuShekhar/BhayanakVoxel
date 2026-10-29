// Random numbers and the sampling distributions of the path tracer. Depends on nothing else, so
// tests can compile it alone (see sampling_tests.rs).

const PI = 3.14159265358979;

// One step of the PCG hash (O'Neill, https://www.pcg-random.org): a good 32-bit mixer.
fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// The start of a random stream for one pixel and one sample under one seed. The same three
// inputs always give the same stream, on every device: this is what makes renders reproducible.
fn seed_rng(pixel: vec2<u32>, sample: u32, seed: u32) -> u32 {
    return pcg(pixel.x + pcg(pixel.y + pcg(sample + pcg(seed))));
}

// The next 32 random bits of a stream (PCG-RXS-M-XS: a linear congruential step, then a hash).
fn next_u32(state: ptr<function, u32>) -> u32 {
    let old = *state;
    *state = old * 747796405u + 2891336453u;
    let word = ((old >> ((old >> 28u) + 4u)) ^ old) * 277803737u;
    return (word >> 22u) ^ word;
}

// Uniform in [0, 1): 24 bits, so every value is exact in an f32 and 1.0 is never reached.
fn next_f32(state: ptr<function, u32>) -> f32 {
    return f32(next_u32(state) >> 8u) * (1.0 / 16777216.0);
}

// A right-handed frame whose z axis is the unit vector `n`, as columns (Duff et al., "Building
// an Orthonormal Basis, Revisited", 2017).
fn basis(n: vec3<f32>) -> mat3x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    return mat3x3<f32>(
        vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x),
        vec3<f32>(b, s + n.y * n.y * a, -n.y),
        n,
    );
}

// A unit vector about `n` with density cos(theta) / pi over the hemisphere: the density of a
// Lambertian surface's reflection, so the cosine and the pi cancel in the estimator.
fn cosine_hemisphere(n: vec3<f32>, u: vec2<f32>) -> vec3<f32> {
    let r = sqrt(u.x);
    let phi = 2.0 * PI * u.y;
    let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - u.x)));
    return basis(n) * local;
}

// A unit vector uniform over the cone of directions within `acos(cos_max)` of the unit vector
// `axis`. With cos_max = 1 it is exactly `axis`.
fn sample_cone(axis: vec3<f32>, cos_max: f32, u: vec2<f32>) -> vec3<f32> {
    let cos_theta = 1.0 - u.x * (1.0 - cos_max);
    let sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
    let phi = 2.0 * PI * u.y;
    let local = vec3<f32>(sin_theta * cos(phi), sin_theta * sin(phi), cos_theta);
    return basis(axis) * local;
}
