// Batched 2D primitives for PiViz: beveled notes, light beams, flames (rising or hanging), additive rects.
//
// Per-vertex data (see draw.rs):
//   local: offset from the quad center, in pixels
//   shape: (half width, half height, param, kind)
//   color: linear HDR color
// Output is premultiplied: alpha 0 means purely additive light.

#import bevy_sprite::mesh2d_functions as mesh_functions

// x: relief strength, y: halo strength, z: hit line y (world), w: top of the screen (world)
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: vec4<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(2) local: vec2<f32>,
    @location(3) shape: vec4<f32>,
    @location(4) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) shape: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) world: vec2<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = mesh_functions::mesh2d_position_local_to_world(world_from_local, vec4(v.position, 1.0));
    out.clip = mesh_functions::mesh2d_position_world_to_clip(world);
    out.local = v.local;
    out.shape = v.shape;
    out.color = v.color;
    out.world = world.xy;
    return out;
}

const HALO_MARGIN: f32 = 16.0;

fn sd_round_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// A raised, rounded bar lit from the upper left.
fn note(in: VertexOutput) -> vec4<f32> {
    let half = in.shape.xy;
    let lit = in.shape.z;
    let relief = params.x;
    let p = in.local;
    let r = min(min(half.x, half.y), 5.0);
    let d = sd_round_box(p, half, r);

    let e = 0.75;
    let grad = vec2(
        sd_round_box(p + vec2(e, 0.0), half, r) - sd_round_box(p - vec2(e, 0.0), half, r),
        sd_round_box(p + vec2(0.0, e), half, r) - sd_round_box(p - vec2(0.0, e), half, r),
    );
    let g = grad / max(length(grad), 1e-4);

    // Bevel: the surface rises over `bevel` pixels from the edge, then a gently curved top.
    let bevel = max(min(half.x * 0.55, 7.0), 1.0);
    let t = clamp(-d / bevel, 0.0, 1.0);
    let tilt = (1.0 - t) * (1.0 - t) * 2.2 * relief;
    let across = clamp(p.x / half.x, -1.0, 1.0) * 0.45 * relief;
    let n = normalize(vec3(g * tilt + vec2(across, 0.0), 1.0));

    let l = normalize(vec3(-0.42, 0.62, 0.66));
    let h = normalize(l + vec3(0.0, 0.0, 1.0));
    let diffuse = max(dot(n, l), 0.0);
    let spec = pow(max(dot(n, h), 0.0), 60.0);

    let base = in.color.rgb;
    let along = clamp(0.5 - 0.5 * p.y / half.y, 0.0, 1.0);
    var col = base * mix(1.0, (0.35 + 0.75 * diffuse) * (0.85 + 0.3 * along), min(relief, 1.0));
    col += base * pow(1.0 - t, 3.0) * 0.6;
    col += mix(base, vec3(1.0), 0.6) * spec * (0.9 + 1.5 * lit) * relief;
    // Dark contour keeps touching notes apart.
    col *= 0.45 + 0.55 * smoothstep(0.0, 1.5, -d);
    // Notes being played burn hotter where they meet the keyboard.
    let heat = lit * exp(-max(in.world.y - params.z, 0.0) / 40.0);
    col += base * heat * 2.5;

    let coverage = clamp(0.5 - d, 0.0, 1.0);
    let reach = 3.0 + 6.0 * lit;
    // Fades out before the quad edge (HALO_MARGIN in draw.rs) so no box is visible.
    let fade = 1.0 - smoothstep(HALO_MARGIN * 0.5, HALO_MARGIN, d);
    let halo = exp(-max(d, 0.0) / reach) * params.y * (0.12 + 0.5 * lit) * fade;
    // Bars dissolve over the top third of the screen instead of being cut by the edge.
    let fade_start = params.w - (params.w - params.z) * 0.35;
    let vanish = 1.0 - smoothstep(fade_start, params.w, in.world.y);
    return vec4(col * coverage + base * halo * (1.0 - coverage), coverage) * vanish;
}

// Vertical light rising from its bottom edge.
fn beam(in: VertexOutput) -> vec4<f32> {
    let half = in.shape.xy;
    let fy = clamp((in.local.y + half.y) / (2.0 * half.y), 0.0, 1.0);
    let fx = in.local.x / half.x;
    let i = pow(1.0 - fy, 2.2) * exp(-fx * fx * 2.5);
    return vec4(in.color.rgb * i, 0.0);
}

fn hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
        mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
        u.y,
    );
}

// Rotating each octave hides the axis-aligned grid of the value noise.
const OCTAVE_ROT = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);

fn fbm(p: vec2<f32>) -> f32 {
    var v = 0.0;
    var a = 0.5;
    var q = p;
    for (var i = 0; i < 5; i++) {
        v += a * value_noise(q);
        q = OCTAVE_ROT * q * 2.03 + vec2(17.0, 9.2);
        a *= 0.5;
    }
    return v;
}

// Flickering flame / colored smoke rising from the bottom edge.
// `param` is the animation time; color alpha is the edge fade: the fraction of the half-width
// over which the sides fade out (1 = single tapering flame, small = wide strip with soft ends).
fn flame(in: VertexOutput, down: bool) -> vec4<f32> {
    let half = in.shape.xy;
    let t = in.shape.z;
    let x = in.world.x;
    // Height above the line; hanging flames measure it downward so they also move away from it.
    let y = select(in.local.y, -in.local.y, down) + half.y;

    // The crest height wanders slowly along the line, so the top is never flat.
    let crest = 0.45 + 0.55 * fbm(vec2(x * 0.007, t * 0.35));
    let fy = clamp(y / (2.0 * half.y * crest), 0.0, 1.0);

    // Single flames narrow toward their tip; every flame reaches exactly zero at the quad sides.
    let edge_fade = in.color.a;
    let taper = max(1.0 - fy * 0.7 * edge_fade, 0.05);
    let fx = in.local.x / (half.x * taper);
    let side = smoothstep(0.0, 1.0, clamp((1.0 - abs(fx)) / edge_fade, 0.0, 1.0));

    // World-space noise, stretched vertically into tongues and curled by a domain warp.
    let p = vec2(x * 0.024, y * 0.016 - t * 1.6);
    let warp = fbm(p * 0.8 + vec2(t * 0.1, -t * 0.3));
    let n = fbm(p + vec2(warp * 1.6 - 0.8, warp * 0.8));
    // The threshold rises with height: only the strongest tongues reach the top, as pointed tips.
    let thr = 0.28 + 0.62 * fy;
    let tongues = smoothstep(thr, thr + 0.18, n) * (1.0 - smoothstep(0.7, 1.0, fy));
    let wisps = smoothstep(0.5, 0.8, n) * (1.0 - fy) * (1.0 - fy) * 0.3;
    let core = exp(-y / 2.0) * 1.2 + exp(-y / 9.0) * 0.35;

    let i = (tongues + wisps + core) * side;
    // The hottest parts drift toward white.
    let col = in.color.rgb * i + vec3(max(i - 0.8, 0.0) * 0.6);
    return vec4(col, 0.0);
}

// Additive rectangle with anti-aliased sides.
fn flat_rect(in: VertexOutput) -> vec4<f32> {
    let ax = clamp(in.shape.x - abs(in.local.x) + 0.5, 0.0, 1.0);
    return vec4(in.color.rgb * ax, 0.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    switch u32(in.shape.w + 0.5) {
        case 0u: { return note(in); }
        case 1u: { return beam(in); }
        case 2u: { return flame(in, false); }
        case 4u: { return flame(in, true); }
        default: { return flat_rect(in); }
    }
}
