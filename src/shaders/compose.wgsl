// Final composition of the bloomed, tone-mapped scene into the window.
//
// - Fades everything out over the window border when the keyboard is hidden, so glows and
//   bloom never end on a hard edge.
// - On a transparent background, additive light (flames, halos, bloom) has color but no alpha:
//   its coverage is derived from its brightness, giving a valid premultiplied image.
// - On the camera background, the scene is projected onto the real keyboard through a
//   homography and screen-blended over the webcam image.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct Params {
    // x: edge fade width (logical px, 0 = off), y: 1 for a transparent background, zw: window size (logical px)
    settings: vec4<f32>,
    // Linear background color the border fades to on opaque backgrounds.
    background: vec4<f32>,
    // x: 1 for the camera background, y: camera brightness, z: 1 when the projection is valid
    camera: vec4<f32>,
    // Camera image within the window, in window uv: xy offset, zw size.
    camera_rect: vec4<f32>,
    // Camera uv -> scene uv homography, by rows.
    to_scene_x: vec4<f32>,
    to_scene_y: vec4<f32>,
    to_scene_w: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: Params;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var scene: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var scene_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var camera: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var camera_sampler: sampler;

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

fn over_camera(uv: vec2<f32>) -> vec4<f32> {
    let cam_uv = (uv - params.camera_rect.xy) / params.camera_rect.zw;
    if any(cam_uv < vec2(0.0)) || any(cam_uv > vec2(1.0)) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    let base = textureSampleLevel(camera, camera_sampler, cam_uv, 0.0).rgb * params.camera.y;

    var light = vec3(0.0);
    let p = vec3(cam_uv, 1.0);
    let w = dot(params.to_scene_w.xyz, p);
    if params.camera.z > 0.5 && w > 1e-6 {
        let s = vec2(dot(params.to_scene_x.xyz, p), dot(params.to_scene_y.xyz, p)) / w;
        if all(s >= vec2(0.0)) && all(s <= vec2(1.0)) {
            // The scene's borders would otherwise end on a hard line in the middle of the image:
            // fade them as without a keyboard, the top (often squeezed by the perspective) more.
            let size = params.settings.zw;
            let px = s * size;
            let edge = min(min(px.x, size.x - px.x), min(px.y, size.y - px.y));
            let fade = smoothstep(0.0, max(params.settings.x, 1.0), edge) * smoothstep(0.0, 0.15, s.y);
            light = clamp(textureSampleLevel(scene, scene_sampler, s, 0.0).rgb, vec3(0.0), vec3(1.0)) * fade;
        }
    }
    // Screen blend on display-encoded values, like the "Screen" blend mode of video tools.
    let a = to_srgb(base);
    let b = to_srgb(light);
    return vec4(to_linear(1.0 - (1.0 - a) * (1.0 - b)), 1.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    if params.camera.x > 0.5 {
        return over_camera(in.uv);
    }
    let c = textureSample(scene, scene_sampler, in.uv);

    let size = params.settings.zw;
    let px = in.uv * size;
    let edge = min(min(px.x, size.x - px.x), min(px.y, size.y - px.y));
    var fade = 1.0;
    if params.settings.x > 0.0 {
        fade = smoothstep(0.0, params.settings.x, edge);
    }

    if params.settings.y > 0.5 {
        let alpha = clamp(max(c.a, max(c.r, max(c.g, c.b))), 0.0, 1.0);
        return vec4(c.rgb, alpha) * fade;
    }
    return vec4(mix(params.background.rgb, c.rgb, fade), 1.0);
}
