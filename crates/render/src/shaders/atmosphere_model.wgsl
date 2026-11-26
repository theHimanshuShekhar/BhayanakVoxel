// The atmosphere's model (Hillaire 2020, "A Scalable and Production Ready Sky and Atmosphere
// Rendering Technique", with the multiple scattering of its section 5): the Planet's air as
// Rayleigh, Mie and ozone layers, and how the lookup tables (LUTs) are indexed. It is pure
// functions of the `atmo` uniform that the including module declares (atmosphere.wgsl writes the
// tables; sky.wgsl reads them), so that the module that fills a table and the modules that read
// it cannot index it differently. Distances are in metres, as the Planet's physics is; the
// shaders of the views work in Voxels and convert at the one place that reads these tables.
//
// The Planet's centre is the origin and a ray is described by its radius `r` (the distance of its
// start from the centre) and the cosine `mu` of the angle between it and the local up there:
// everything is rotationally symmetric about the up, so two numbers are the whole position and
// direction. The sun is `mu_s`, its own cosine with the up, and `nu`, the cosine between the ray
// and the sun.

const ATMO_PI = 3.14159265358979;

struct Atmo {
    // The ground sphere's radius, the top of the atmosphere's radius, the camera's radius (all in
    // metres from the Planet's centre), then the image's aspect ratio (for the aerial perspective
    // volume, whose rays are the camera's).
    radii: vec4<f32>,
    // Rayleigh scattering per metre at the ground (rgb), then its scale height in metres.
    rayleigh: vec4<f32>,
    // Mie scattering per metre at the ground (rgb), then its scale height.
    mie_scattering: vec4<f32>,
    // Mie absorption per metre at the ground (rgb), then the phase function's asymmetry g.
    mie_absorption: vec4<f32>,
    // Ozone absorption per metre at the layer's centre (rgb), then the centre's height.
    ozone: vec4<f32>,
    // The unit vector towards the sun, then the ozone layer's half width in metres.
    sun_dir: vec4<f32>,
    // The local up (a unit vector), then the height of the observer that the sun-free table is
    // for (a surface: a couple of metres above the ground).
    up: vec4<f32>,
    // The sun's light above the atmosphere: the radiance of a surface that faces it, as a
    // multiple of the surface's colour (look.rs's sun is this, less what the air takes), rgb.
    sun_light: vec4<f32>,
    // The ground's albedo for the multiple scattering's bounce off it, rgb.
    ground: vec4<f32>,
    // The look's tint (atmosphere.rs `Tint`): what the sky's radiance is multiplied by at the
    // horizon (rgb) and the extra haze's density per metre (w), at the zenith (rgb), and how far
    // the tinted light is taken toward grey, which makes it pastel (w).
    tint_horizon: vec4<f32>,
    tint_zenith: vec4<f32>,
    // The night sky's own light (atmosphere.rs `NightFloor`, #203): its radiance at the horizon
    // (rgb) and how much of it there is, 0 (day) to 1 (night) (w), then at the zenith (rgb).
    night_horizon: vec4<f32>,
    night_zenith: vec4<f32>,
}

// The tables' sizes. Keep in step with atmosphere.rs.
const TRANSMITTANCE_SIZE = vec2<f32>(256.0, 64.0);
const MULTISCATTER_SIZE = 32.0;
const SKY_VIEW_SIZE = vec2<f32>(192.0, 108.0);
// The sun-free table: azimuth from the sun (0 to pi, mirrored) by the sine of the elevation.
const GROUND_SKY_W = 64u;
const GROUND_SKY_H = 32u;
// The aerial perspective volume's size on each axis.
const AERIAL_SLICES = 32.0;
// Its depth is a logarithm of the distance from where the ray enters the air:
// w = ln(1 + t / AERIAL_T_REF) / ln(1 + AERIAL_T_CAP / AERIAL_T_REF), so a slice is half as far
// again as the one before and the first is half a metre out.
const AERIAL_T_REF = 1.0;
const AERIAL_T_CAP = 400000.0;
// ln(1 + AERIAL_T_CAP / AERIAL_T_REF).
const AERIAL_LOG_SPAN = 12.899222;

// A table's coordinate that reaches the texel centres at the ends, which a linear filter needs.
fn to_sub_uv(u: f32, n: f32) -> f32 {
    return (u * (n - 1.0) + 0.5) / n;
}

fn from_sub_uv(u: f32, n: f32) -> f32 {
    return (u * n - 0.5) / (n - 1.0);
}

// The look's tint on the sky's radiance `l` in a direction whose cosine with the up is `mu` (the
// sine of its elevation): `l` multiplied by the horizon's colour at and below the horizon, by the
// zenith's overhead, and by a smoothstep of the sine between (a soft, wide gradient, with no band
// at the horizon), and then taken part of the way to the grey of the same luminance, which is what
// makes the sky milky and pastel.
fn tint_sky(l: vec3<f32>, mu: f32) -> vec3<f32> {
    let s = clamp(mu, 0.0, 1.0);
    let c = l * mix(atmo.tint_horizon.xyz, atmo.tint_zenith.xyz, s * s * (3.0 - 2.0 * s));
    return mix(c, vec3<f32>(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722))), atmo.tint_zenith.w);
}

// The altitude of radius r: below the ground it is the ground's.
fn altitude(r: f32) -> f32 {
    return max(r - atmo.radii.x, 0.0);
}

// The radius of the point t along the ray (r, mu): a sum of squares, so it keeps its precision
// close to the ground, where r is 320 000 and the altitude a metre.
fn radius_at(r: f32, mu: f32, t: f32) -> f32 {
    let s = sqrt(max(1.0 - mu * mu, 0.0));
    return length(vec2<f32>(r * mu + t, r * s));
}

// Where the ray (r, mu) meets the sphere of `radius` about the centre: the near and the far root
// as distances along it, or (-1, -1) if it never does. A negative root is behind the start.
fn sphere_roots(r: f32, mu: f32, radius: f32) -> vec2<f32> {
    let s = sqrt(max(1.0 - mu * mu, 0.0));
    let disc = (radius - r * s) * (radius + r * s);
    if disc < 0.0 {
        return vec2<f32>(-1.0, -1.0);
    }
    let q = sqrt(disc);
    let b = -r * mu;
    return vec2<f32>(b - q, b + q);
}

// The part of the ray (r, mu) that is in air: from where it enters the atmosphere (0 if it starts
// inside) to where it leaves it or meets the ground. t1 <= t0 if there is none.
fn air_segment(r: f32, mu: f32) -> vec2<f32> {
    let top = sphere_roots(r, mu, atmo.radii.y);
    if top.y <= 0.0 {
        return vec2<f32>(0.0, 0.0);
    }
    let t0 = max(top.x, 0.0);
    var t1 = top.y;
    let ground = sphere_roots(r, mu, atmo.radii.x);
    if ground.x > t0 {
        t1 = min(t1, ground.x);
    }
    return vec2<f32>(t0, t1);
}

// How far into the air a ray goes for the night's floor to be all there, in metres: a ray that
// meets no air (the space over a Planet seen from orbit) has none of the floor, one that crosses
// the air has all of it, and a graze of the limb a part.
const NIGHT_AIR_SCALE = 15000.0;

// The night sky's own light along the ray (r, mu): a lilac gradient by the elevation (the sine, as
// the tint's), as much of it as the night level says and as the ray has air. Added to the tinted
// sky wherever the sky is made, so the sky, the haze that far ground fades into and what a surface
// sees of the sky all hold it, and both views read it from the tables.
fn night_floor(r: f32, mu: f32) -> vec3<f32> {
    let level = atmo.night_horizon.w;
    if level <= 0.0 {
        return vec3<f32>(0.0);
    }
    let seg = air_segment(r, mu);
    let air = 1.0 - exp(-max(seg.y - seg.x, 0.0) / NIGHT_AIR_SCALE);
    let s = clamp(mu, 0.0, 1.0);
    let colour = mix(atmo.night_horizon.xyz, atmo.night_zenith.xyz, s * s * (3.0 - 2.0 * s));
    return colour * (level * air);
}

struct Medium {
    rayleigh: vec3<f32>,
    mie: vec3<f32>,
    scattering: vec3<f32>,
    extinction: vec3<f32>,
}

// The air at altitude `h` metres.
fn medium_at(h: f32) -> Medium {
    let dens_r = exp(-h / atmo.rayleigh.w);
    let dens_m = exp(-h / atmo.mie_scattering.w);
    let dens_o = max(1.0 - abs(h - atmo.ozone.w) / atmo.sun_dir.w, 0.0);
    var m: Medium;
    m.rayleigh = atmo.rayleigh.xyz * dens_r;
    m.mie = atmo.mie_scattering.xyz * dens_m;
    m.scattering = m.rayleigh + m.mie;
    m.extinction = m.scattering + atmo.mie_absorption.xyz * dens_m + atmo.ozone.xyz * dens_o;
    return m;
}

fn rayleigh_phase(c: f32) -> f32 {
    return 3.0 / (16.0 * ATMO_PI) * (1.0 + c * c);
}

// Cornette and Shanks's phase function for Mie scattering with asymmetry g.
fn mie_phase(c: f32, g: f32) -> f32 {
    let k = 3.0 / (8.0 * ATMO_PI) * (1.0 - g * g) / (2.0 + g * g);
    return k * (1.0 + c * c) / pow(max(1.0 + g * g - 2.0 * g * c, 1e-4), 1.5);
}

// ---- The transmittance table: the air's transmittance from radius r, along zenith cosine mu, to
// the top of the atmosphere (Bruneton and Neyret 2008's mapping, which spends its texels near the
// horizon, where the path changes fastest).

fn transmittance_uv(r: f32, mu: f32) -> vec2<f32> {
    let bottom = atmo.radii.x;
    let top = atmo.radii.y;
    let h = sqrt(max((top - bottom) * (top + bottom), 0.0));
    let rho = sqrt(max((r - bottom) * (r + bottom), 0.0));
    let s = sqrt(max(1.0 - mu * mu, 0.0));
    let disc = max((top - r * s) * (top + r * s), 0.0);
    let d = max(-r * mu + sqrt(disc), 0.0);
    let d_min = top - r;
    let d_max = rho + h;
    return vec2<f32>(clamp((d - d_min) / max(d_max - d_min, 1e-3), 0.0, 1.0), clamp(rho / h, 0.0, 1.0));
}

// The radius and zenith cosine of a texel of the transmittance table, (x_mu, x_r) in 0..1.
fn transmittance_params(uv: vec2<f32>) -> vec2<f32> {
    let bottom = atmo.radii.x;
    let top = atmo.radii.y;
    let h = sqrt(max((top - bottom) * (top + bottom), 0.0));
    let rho = h * uv.y;
    let r = sqrt(rho * rho + bottom * bottom);
    let d_min = top - r;
    let d_max = rho + h;
    let d = d_min + uv.x * (d_max - d_min);
    var mu = 1.0;
    if d > 0.0 {
        mu = clamp((h * h - rho * rho - d * d) / (2.0 * r * d), -1.0, 1.0);
    }
    return vec2<f32>(r, mu);
}

// ---- The multiple scattering table: Psi(h, mu_s), by the sun's cosine and the altitude (cubic,
// to give the air near the ground, which is where the thick haze is, texels of its own).

fn ms_height_span() -> f32 {
    return atmo.radii.y - atmo.radii.x;
}

fn multiscatter_uv(h: f32, mu_s: f32) -> vec2<f32> {
    let v = pow(clamp(h / ms_height_span(), 0.0, 1.0), 1.0 / 3.0);
    return vec2<f32>(clamp(mu_s * 0.5 + 0.5, 0.0, 1.0), v);
}

// ---- The sky-view table: the sky from the camera's radius, by the azimuth from the sun (0 to pi)
// and a latitude whose texels crowd round the horizon, where the sky changes fastest.

// The angle below the horizontal at which the horizon is seen from radius r, and the horizon's
// angle from the zenith.
fn horizon_angles(r: f32) -> vec2<f32> {
    let bottom = atmo.radii.x;
    let ground_distance = sqrt(max((r - bottom) * (r + bottom), 0.0));
    let beta = acos(clamp(ground_distance / r, 0.0, 1.0));
    return vec2<f32>(beta, ATMO_PI - beta);
}

// How much of the sky below the horizon is the sky at the horizon, as a ground camera sees it (the
// ground is not in the table: where nothing is drawn there, the air is the horizon's too, and the
// ground dissolves into it), 1 near the ground and 0 high over it, where below the horizon is the
// planet seen through its air.
fn horizon_fill() -> f32 {
    return 1.0 - smoothstep(300.0, 3000.0, atmo.radii.z - atmo.radii.x);
}

// How far short of the horizon, in radians, the sky's colour is held for a camera near the ground:
// the rays that graze the horizon cross the most air at the ground and are a thin line brighter (or
// darker) than the sky a degree above them, and a hard line is not what the look wants of a
// horizon. The hold fades out with `horizon_fill`: it is an angle, so high over the Planet it would
// hold a band of tangent altitudes tens of kilometres thick, which is the limb.
const HORIZON_SOFT: f32 = 0.012;

// The angle from the zenith to look the sky up at instead of `angle`, from radius r: the same, but
// held short of the horizon (below).
fn soft_angle(r: f32, angle: f32) -> f32 {
    return min(angle, horizon_angles(r).y - HORIZON_SOFT * horizon_fill());
}

fn sky_view_uv(r: f32, mu: f32, dphi: f32) -> vec2<f32> {
    let h = horizon_angles(r);
    let angle = acos(clamp(mu, -1.0, 1.0));
    var v: f32;
    if angle < h.y {
        v = (1.0 - sqrt(1.0 - angle / h.y)) * 0.5;
    } else {
        v = 0.5 + sqrt(clamp((angle - h.y) / max(h.x, 1e-6), 0.0, 1.0)) * 0.5;
    }
    return vec2<f32>(clamp(dphi / ATMO_PI, 0.0, 1.0), v);
}

// The zenith cosine and the azimuth from the sun of a texel (u, v) in 0..1.
fn sky_view_params(r: f32, uv: vec2<f32>) -> vec2<f32> {
    let h = horizon_angles(r);
    var angle: f32;
    if uv.y < 0.5 {
        let c = 1.0 - 2.0 * uv.y;
        angle = (1.0 - c * c) * h.y;
    } else {
        let c = 2.0 * uv.y - 1.0;
        angle = h.y + c * c * h.x;
    }
    return vec2<f32>(cos(angle), uv.x * ATMO_PI);
}

// ---- Directions about the local up, measured from the sun's side.

// The unit vector along the horizon towards the sun's azimuth: the sky's azimuth is measured from
// it. With the sun straight overhead any horizontal direction will do.
fn sun_horizontal() -> vec3<f32> {
    let up = atmo.up.xyz;
    let sun = atmo.sun_dir.xyz;
    let flat = sun - up * dot(sun, up);
    if dot(flat, flat) > 1e-8 {
        return normalize(flat);
    }
    let other = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(up.x) > 0.9);
    return normalize(cross(up, other));
}

// The cosine of the angle between two directions, given their zenith cosines and the angle
// between their azimuths.
fn cos_between(mu_a: f32, mu_b: f32, dphi: f32) -> f32 {
    return mu_a * mu_b + sqrt(max((1.0 - mu_a * mu_a) * (1.0 - mu_b * mu_b), 0.0)) * cos(dphi);
}

// The zenith cosine of `dir` and its azimuth from the sun, 0 to pi (the sky is the same on both
// sides of the sun's vertical plane).
fn dir_to_sky_angles(dir: vec3<f32>) -> vec2<f32> {
    let up = atmo.up.xyz;
    let mu = clamp(dot(dir, up), -1.0, 1.0);
    let flat = dir - up * mu;
    var dphi = 0.0;
    if dot(flat, flat) > 1e-10 {
        dphi = acos(clamp(dot(normalize(flat), sun_horizontal()), -1.0, 1.0));
    }
    return vec2<f32>(mu, dphi);
}

// What a texel of the sun-free table weighs (the table is `ground_sky`, atmosphere.wgsl) in the sampling: its luminance, and a little more so
// that no direction has probability 0.
fn table_weight(rgb: vec3<f32>) -> f32 {
    return max(dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0) + 1e-7;
}

// ---- The aerial perspective volume: the light the air adds along a camera ray, and what it lets
// through, to a distance. Its depth is `AERIAL_T_REF * (exp(w * AERIAL_LOG_SPAN) - 1)` metres from
// where the ray enters the air, w in 0..1 (the slice k holds w = (k + 1) / AERIAL_SLICES, so the
// air a ray is in is counted from the camera when the camera is in it).

fn aerial_distance(w: f32) -> f32 {
    return AERIAL_T_REF * (exp(w * AERIAL_LOG_SPAN) - 1.0);
}

fn aerial_depth(distance: f32) -> f32 {
    return log(1.0 + max(distance, 0.0) / AERIAL_T_REF) / AERIAL_LOG_SPAN;
}
