// Random numbers and the sampling distributions of the path tracer. Depends on nothing else, so
// tests can compile it alone (see sampling_tests.rs).

const PI = 3.14159265358979;

// One step of the PCG hash (O'Neill, https://www.pcg-random.org): a good 32-bit mixer.
fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// The sample sequence of the Reference view (ADR 0005): hash-based Owen-scrambled Sobol
// (Burley, "Practical Hash-based Owen Scrambling", JCGT 2020), computed from whole numbers
// only, from three inputs: the pixel (and the seed, folded into `pixel_seed`), the sample's index
// and the dimension. Nothing is stored and nothing is read from a texture, so the same inputs
// give the same bits on every device. A path asks for its dimensions by number, in any order,
// and a path that ends early simply never asks for the later ones.
//
// The first SOBOL_GROUPS * 4 = 88 dimensions are Sobol, in groups of four. Each group has its own scramble,
// so the groups are not correlated with each other (Burley, section 4), and within a group the
// four dimensions keep Sobol's stratification: the first two are a (0,2)-sequence, so 4^k
// consecutive samples put one point in each cell of every 2^i by 2^(k-i) grid. Dimensions past
// those are hashed random numbers. `white` replaces all of them by hashed random numbers:
// white noise, which tests use as the plain Monte Carlo baseline.
const SOBOL_GROUPS = 22u;

// The scramble of one pixel under one seed. Whole numbers, so it is the same on every device.
fn pixel_hash(pixel: vec2<u32>, seed: u32) -> u32 {
    return pcg(pixel.x + pcg(pixel.y + pcg(seed)));
}

// Uniform in [0, 1) from 32 random bits: 24 bits, so every value is exact in an f32 and 1.0 is
// never reached.
fn unit_f32(bits: u32) -> f32 {
    return f32(bits >> 8u) * (1.0 / 16777216.0);
}

// A hashed random number for one dimension of one sample: white noise.
fn hashed_f32(pixel_seed: u32, sample: u32, dim: u32) -> f32 {
    return unit_f32(pcg(pcg(pixel_seed ^ pcg(dim + 0x9E3779B9u)) + sample));
}

// A hash of a seed and a small number, to make one seed for each of them.
fn hash_combine(seed: u32, i: u32) -> u32 {
    return pcg(seed ^ (i * 0x9E3779B9u));
}

// Laine, Karras and Aila's hash that is a bit-level random permutation: each bit of the result
// is the bit of `x` flipped or not by a function of the bits below it (Burley, listing 2).
fn laine_karras_permutation(x: u32, seed: u32) -> u32 {
    var v = x + seed;
    v ^= v * 0x6c50b47cu;
    v ^= v * 0xb82f1e52u;
    v ^= v * 0xc7afe638u;
    v ^= v * 0x8d22f6e6u;
    return v;
}

// An Owen scramble of `x` read as a binary fraction: each digit is flipped or not by a function
// of the digits before it (the bits above it), which `reverseBits` turns into the carries of the
// hash above. Applied to a sample's index it shuffles the order of the samples, mapping every
// block of 2^k indices that starts at a multiple of 2^k onto one such block, so the sequence
// stays progressive.
fn nested_uniform_scramble(x: u32, seed: u32) -> u32 {
    return reverseBits(laine_karras_permutation(reverseBits(x), seed));
}

// Direction numbers of the first four dimensions of Sobol's sequence (Joe and Kuo's
// new-joe-kuo-6.21201 for dimensions 2 to 4; the first is the van der Corput sequence), one
// vec4 for each bit of the index. A test recomputes them from Joe and Kuo's recurrence.
const SOBOL = array<vec4<u32>, 32>(
    vec4<u32>(0x80000000u, 0x80000000u, 0x80000000u, 0x80000000u),
    vec4<u32>(0x40000000u, 0xC0000000u, 0xC0000000u, 0xC0000000u),
    vec4<u32>(0x20000000u, 0xA0000000u, 0x60000000u, 0x20000000u),
    vec4<u32>(0x10000000u, 0xF0000000u, 0x90000000u, 0x50000000u),
    vec4<u32>(0x08000000u, 0x88000000u, 0xE8000000u, 0xF8000000u),
    vec4<u32>(0x04000000u, 0xCC000000u, 0x5C000000u, 0x74000000u),
    vec4<u32>(0x02000000u, 0xAA000000u, 0x8E000000u, 0xA2000000u),
    vec4<u32>(0x01000000u, 0xFF000000u, 0xC5000000u, 0x93000000u),
    vec4<u32>(0x00800000u, 0x80800000u, 0x68800000u, 0xD8800000u),
    vec4<u32>(0x00400000u, 0xC0C00000u, 0x9CC00000u, 0x25400000u),
    vec4<u32>(0x00200000u, 0xA0A00000u, 0xEE600000u, 0x59E00000u),
    vec4<u32>(0x00100000u, 0xF0F00000u, 0x55900000u, 0xE6D00000u),
    vec4<u32>(0x00080000u, 0x88880000u, 0x80680000u, 0x78080000u),
    vec4<u32>(0x00040000u, 0xCCCC0000u, 0xC09C0000u, 0xB40C0000u),
    vec4<u32>(0x00020000u, 0xAAAA0000u, 0x60EE0000u, 0x82020000u),
    vec4<u32>(0x00010000u, 0xFFFF0000u, 0x90550000u, 0xC3050000u),
    vec4<u32>(0x00008000u, 0x80008000u, 0xE8808000u, 0x208F8000u),
    vec4<u32>(0x00004000u, 0xC000C000u, 0x5CC0C000u, 0x51474000u),
    vec4<u32>(0x00002000u, 0xA000A000u, 0x8E606000u, 0xFBEA2000u),
    vec4<u32>(0x00001000u, 0xF000F000u, 0xC5909000u, 0x75D93000u),
    vec4<u32>(0x00000800u, 0x88008800u, 0x6868E800u, 0xA0858800u),
    vec4<u32>(0x00000400u, 0xCC00CC00u, 0x9C9C5C00u, 0x914E5400u),
    vec4<u32>(0x00000200u, 0xAA00AA00u, 0xEEEE8E00u, 0xDBE79E00u),
    vec4<u32>(0x00000100u, 0xFF00FF00u, 0x5555C500u, 0x25DB6D00u),
    vec4<u32>(0x00000080u, 0x80808080u, 0x8000E880u, 0x58800080u),
    vec4<u32>(0x00000040u, 0xC0C0C0C0u, 0xC0005CC0u, 0xE54000C0u),
    vec4<u32>(0x00000020u, 0xA0A0A0A0u, 0x60008E60u, 0x79E00020u),
    vec4<u32>(0x00000010u, 0xF0F0F0F0u, 0x9000C590u, 0xB6D00050u),
    vec4<u32>(0x00000008u, 0x88888888u, 0xE8006868u, 0x800800F8u),
    vec4<u32>(0x00000004u, 0xCCCCCCCCu, 0x5C009C9Cu, 0xC00C0074u),
    vec4<u32>(0x00000002u, 0xAAAAAAAAu, 0x8E00EEEEu, 0x200200A2u),
    vec4<u32>(0x00000001u, 0xFFFFFFFFu, 0xC5005555u, 0x50050093u),
);

// The 4D Sobol point of `index`, as four fractions of 2^32.
fn sobol4(index: u32) -> vec4<u32> {
    var point = vec4<u32>(0u);
    var bits = index;
    for (var k = 0u; bits != 0u; k++) {
        if (bits & 1u) != 0u {
            point ^= SOBOL[k];
        }
        bits >>= 1u;
    }
    return point;
}

// Dimensions 4 * group to 4 * group + 3 of sample `sample`, each in [0, 1). A group past the
// last Sobol group is hashed.
fn sample_group(pixel_seed: u32, sample: u32, group: u32, white: bool) -> vec4<f32> {
    if white || group >= SOBOL_GROUPS {
        return vec4<f32>(
            hashed_f32(pixel_seed, sample, group * 4u),
            hashed_f32(pixel_seed, sample, group * 4u + 1u),
            hashed_f32(pixel_seed, sample, group * 4u + 2u),
            hashed_f32(pixel_seed, sample, group * 4u + 3u),
        );
    }
    let seed = hash_combine(pixel_seed, group);
    let point = sobol4(nested_uniform_scramble(sample, seed));
    return vec4<f32>(
        unit_f32(nested_uniform_scramble(point.x, hash_combine(seed, 0u))),
        unit_f32(nested_uniform_scramble(point.y, hash_combine(seed, 1u))),
        unit_f32(nested_uniform_scramble(point.z, hash_combine(seed, 2u))),
        unit_f32(nested_uniform_scramble(point.w, hash_combine(seed, 3u))),
    );
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

// ---- A rectangle, sampled over the solid angle it covers.
//
// Ureña, Fajardo and King, "An Area-Preserving Parametrization for Spherical Rectangles" (EGSR
// 2013). The rectangle is `[x0, x1] x [y0, y1]` in the plane `z = z0` of a frame whose origin is
// the point it is seen from, with `z0 < 0` (the caller flips the frame so). `sph_quad` is
// the work that is done once for a rectangle and a point (its solid angle too), `sph_quad_sample`
// takes a point of it, in `(x, y)`, for two numbers in 0..1: every part of the solid angle is as
// likely as every other, so the density over solid angle is 1 / `solid_angle` wherever the
// direction points. A rectangle that is edge-on (`|z0|` under 1e-4) or too small to matter has a
// solid angle of 0, and is never sampled. Whoever samples and whoever works out a density must
// use these two functions, or the two will not agree on what has a solid angle.

struct SphQuad {
    x0: f32,
    y0: f32,
    z0: f32,
    x1: f32,
    y1: f32,
    // The z of the normals of two edges and the angle that the first sample number starts from.
    b0: f32,
    b1: f32,
    k: f32,
    solid_angle: f32,
}

// Rectangles that cover less than this many steradians are not sampled (a pixel of a 4K screen is
// 1e-6 or so): the light that is left out is under a thousandth of what a surface of that
// emitter gets, and the bounce rays count it.
const QUAD_MIN_SOLID_ANGLE = 1e-7;

fn safe_acos(x: f32) -> f32 {
    return acos(clamp(x, -1.0, 1.0));
}

fn sph_quad(x0: f32, y0: f32, z0: f32, x1: f32, y1: f32) -> SphQuad {
    var q: SphQuad;
    q.x0 = x0;
    q.y0 = y0;
    q.z0 = z0;
    q.x1 = x1;
    q.y1 = y1;
    q.solid_angle = 0.0;
    if abs(z0) < 1e-4 {
        return q;
    }
    let v00 = vec3<f32>(x0, y0, z0);
    let v01 = vec3<f32>(x0, y1, z0);
    let v10 = vec3<f32>(x1, y0, z0);
    let v11 = vec3<f32>(x1, y1, z0);
    let n0 = normalize(cross(v00, v10));
    let n1 = normalize(cross(v10, v11));
    let n2 = normalize(cross(v11, v01));
    let n3 = normalize(cross(v01, v00));
    let g0 = safe_acos(-dot(n0, n1));
    let g1 = safe_acos(-dot(n1, n2));
    let g2 = safe_acos(-dot(n2, n3));
    let g3 = safe_acos(-dot(n3, n0));
    q.b0 = n0.z;
    q.b1 = n2.z;
    q.k = 2.0 * PI - g2 - g3;
    let area = g0 + g1 - q.k;
    if area >= QUAD_MIN_SOLID_ANGLE {
        q.solid_angle = area;
    }
    return q;
}

// The point `(x, y)` of the rectangle for `u` in 0..1 x 0..1.
fn sph_quad_sample(q: SphQuad, u: vec2<f32>) -> vec2<f32> {
    // The first number places the point along x.
    let au = u.x * q.solid_angle + q.k;
    let fu = (cos(au) * q.b0 - q.b1) / sin(au);
    let cu = clamp(select(-1.0, 1.0, fu > 0.0) / sqrt(fu * fu + q.b0 * q.b0), -1.0, 1.0);
    var xu = -(cu * q.z0) / sqrt(max(1.0 - cu * cu, 1e-12));
    xu = clamp(xu, q.x0, q.x1);
    // The second places it along y, for that x.
    let d = sqrt(xu * xu + q.z0 * q.z0);
    let h0 = q.y0 / sqrt(d * d + q.y0 * q.y0);
    let h1 = q.y1 / sqrt(d * d + q.y1 * q.y1);
    let hv = h0 + u.y * (h1 - h0);
    let hv2 = hv * hv;
    let yv = select(q.y1, (hv * d) / sqrt(1.0 - hv2), hv2 < 1.0 - 1e-6);
    return vec2<f32>(xu, yv);
}
