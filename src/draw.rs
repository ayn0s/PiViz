//! Batched drawing of every note, beam and flame through one mesh and one custom shader.
//!
//! Plugins push primitives into `DrawList` during the frame; at the end the mesh buffers
//! are refilled in place (their capacity is kept, so no allocation in steady state).

use crate::colors::Palette;
use crate::keyboard::Layout;
use crate::settings::{Background, Settings, glow};
use crate::shared::{FIRST_KEY, Frame, LAST_KEY};
use bevy::asset::{RenderAssetUsages, load_internal_asset, uuid_handle};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendState, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};

/// `shaders/draw.wgsl`, embedded in the binary under this fixed id.
const DRAW_SHADER: Handle<Shader> = uuid_handle!("6b0f2a8e-3c1d-4e57-9a62-5f1e8d7c4b30");
/// Z of the batched mesh: behind the keys, glows and particles.
const Z_DRAW: f32 = 5.0;
/// Extra pixels around notes for their halo (same constant in draw.wgsl).
const HALO_MARGIN: f32 = 16.0;

/// Kind of a note bar (beveled, with a halo).
const KIND_NOTE: f32 = 0.0;
/// Kind of a beam of light rising from its bottom edge.
const KIND_BEAM: f32 = 1.0;
/// Kind of a flame rising from its bottom edge.
const KIND_FLAME: f32 = 2.0;
/// Kind of an additive rectangle.
const KIND_RECT: f32 = 3.0;
/// Kind of a flame hanging down from its top edge.
const KIND_FLAME_DOWN: f32 = 4.0;

/// One quad of the mesh. Usually a whole primitive; a gradient strip is split into several quads
/// that share the strip's shape (`half`, measured from the strip's center) so the shader draws
/// one continuous primitive across them.
#[derive(Clone, Copy)]
struct Quad {
    /// Center and half size of the quad itself.
    center: Vec2,
    /// Half size of the quad itself, margin included.
    extent: Vec2,
    /// Offset of the quad's center from the primitive's center, and the primitive's half size.
    offset: Vec2,
    /// Half size of the primitive this quad belongs to.
    half: Vec2,
    /// Meaning depends on `kind`: held flag for notes, animation time for flames.
    param: f32,
    /// One of the `KIND_*` constants.
    kind: f32,
    /// Colors of the quad's left and right edges, interpolated across it.
    left: LinearRgba,
    /// Color of the right edge.
    right: LinearRgba,
}

/// Gradient strips are split into this many quads (colors are interpolated linearly in between).
const STRIP_SEGMENTS: usize = 32;

/// Primitives to draw this frame, in back-to-front order.
#[derive(Resource, Default)]
pub struct DrawList(Vec<Quad>);

impl DrawList {
    /// A beveled note. `active` notes are hotter and have a wider halo.
    pub fn note(&mut self, center: Vec2, size: Vec2, color: LinearRgba, active: bool) {
        self.push(
            center,
            size / 2.0,
            HALO_MARGIN,
            f32::from(active),
            KIND_NOTE,
            color,
        );
    }

    /// Light rising from `bottom` over `height` pixels.
    pub fn beam(&mut self, bottom: Vec2, width: f32, height: f32, color: LinearRgba) {
        self.beam_strip(bottom, width, height, |_| color);
    }

    /// The same light, its color varying along x: `color(world x)`.
    pub fn beam_strip(
        &mut self,
        bottom: Vec2,
        width: f32,
        height: f32,
        color: impl Fn(f32) -> LinearRgba,
    ) {
        let half = Vec2::new(width, height) / 2.0;
        self.push_strip(bottom + Vec2::new(0.0, half.y), half, 0.0, KIND_BEAM, color);
    }

    /// Animated flame rising from `bottom`.
    /// `edge_fade` is the fraction of the half-width over which the sides fade out:
    /// 1.0 gives a single tapering flame, a small value a wide strip with soft ends.
    pub fn flame(
        &mut self,
        bottom: Vec2,
        size: Vec2,
        time: f32,
        color: LinearRgba,
        edge_fade: f32,
    ) {
        self.flame_strip(bottom, size, time, edge_fade, false, |_| color);
    }

    /// The same flame, hanging down from `top` (used below the line when the keyboard is hidden).
    pub fn flame_down(
        &mut self,
        top: Vec2,
        size: Vec2,
        time: f32,
        color: LinearRgba,
        edge_fade: f32,
    ) {
        self.flame_strip(top, size, time, edge_fade, true, |_| color);
    }

    /// A flame whose color varies along x: `color(world x)`. Rises from `base`, or hangs down
    /// from it when `down`.
    pub fn flame_strip(
        &mut self,
        base: Vec2,
        size: Vec2,
        time: f32,
        edge_fade: f32,
        down: bool,
        color: impl Fn(f32) -> LinearRgba,
    ) {
        let half = size / 2.0;
        let (center, kind) = if down {
            (base - Vec2::new(0.0, half.y), KIND_FLAME_DOWN)
        } else {
            (base + Vec2::new(0.0, half.y), KIND_FLAME)
        };
        // Color alpha is unused by additive primitives: it carries the edge fade.
        let fade = edge_fade.clamp(0.001, 1.0);
        self.push_strip(center, half, time, kind, |x| color(x).with_alpha(fade));
    }

    /// Additive rectangle.
    pub fn rect(&mut self, center: Vec2, size: Vec2, color: LinearRgba) {
        self.push(center, size / 2.0, 1.0, 0.0, KIND_RECT, color);
    }

    /// One quad covering the primitive, plus `margin` pixels around it (for halos).
    fn push(
        &mut self,
        center: Vec2,
        half: Vec2,
        margin: f32,
        param: f32,
        kind: f32,
        color: LinearRgba,
    ) {
        self.0.push(Quad {
            center,
            extent: half + Vec2::splat(margin),
            offset: Vec2::ZERO,
            half,
            param,
            kind,
            left: color,
            right: color,
        });
    }

    /// The primitive split into vertical slices, each edge taking `color(world x)`. A constant
    /// color needs no split: one quad.
    fn push_strip(
        &mut self,
        center: Vec2,
        half: Vec2,
        param: f32,
        kind: f32,
        color: impl Fn(f32) -> LinearRgba,
    ) {
        let left = center.x - half.x;
        let width = 2.0 * half.x / STRIP_SEGMENTS as f32;
        let edges: [LinearRgba; STRIP_SEGMENTS + 1] =
            std::array::from_fn(|i| color(left + width * i as f32));
        if edges.iter().all(|c| *c == edges[0]) {
            self.0.push(Quad {
                center,
                extent: half,
                offset: Vec2::ZERO,
                half,
                param,
                kind,
                left: edges[0],
                right: edges[0],
            });
            return;
        }
        for (i, pair) in edges.windows(2).enumerate() {
            let quad_center = Vec2::new(left + width * (i as f32 + 0.5), center.y);
            self.0.push(Quad {
                center: quad_center,
                extent: Vec2::new(width / 2.0, half.y),
                offset: quad_center - center,
                half,
                param,
                kind,
                left: pair[0],
                right: pair[1],
            });
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
/// Material of the batched mesh.
pub struct DrawMaterial {
    /// x: relief, y: halo, z: hit line y, w: top of the screen.
    #[uniform(0)]
    params: Vec4,
}

impl Material2d for DrawMaterial {
    fn vertex_shader() -> ShaderRef {
        DRAW_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        DRAW_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    // Premultiplied blending lets one shader output both opaque notes and additive light.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING);
            }
        }
        Ok(())
    }
}

#[derive(Resource)]
/// Handles of the batched mesh and its material.
struct DrawHandles {
    /// The mesh rebuilt every frame.
    mesh: Handle<Mesh>,
    /// Its material (shader parameters).
    material: Handle<DrawMaterial>,
}

/// Batched drawing of notes, beams and flames.
pub struct DrawPlugin;

impl Plugin for DrawPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, DRAW_SHADER, "shaders/draw.wgsl", Shader::from_wgsl);
        app.add_plugins(Material2dPlugin::<DrawMaterial>::default())
            .insert_resource(DrawList(Vec::with_capacity(2048)))
            .add_systems(Startup, setup)
            .add_systems(Update, begin_frame.in_set(Frame::Clock))
            .add_systems(Update, (sync_material, build_mesh).in_set(Frame::Draw));
    }
}

/// Creates the mesh with preallocated buffers and spawns it.
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<DrawMaterial>>,
) {
    // Kept in the main world so it can be refilled every frame.
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        Vec::<[f32; 3]>::with_capacity(8192),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::with_capacity(8192))
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_TANGENT,
        Vec::<[f32; 4]>::with_capacity(8192),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::with_capacity(8192))
    .with_inserted_indices(Indices::U32(Vec::with_capacity(12288)));
    fill_mesh(&mut mesh, &[EMPTY]);
    let handles = DrawHandles {
        mesh: meshes.add(mesh),
        material: materials.add(DrawMaterial { params: Vec4::ONE }),
    };
    commands.spawn((
        Mesh2d(handles.mesh.clone()),
        MeshMaterial2d(handles.material.clone()),
        Transform::from_xyz(0.0, 0.0, Z_DRAW),
        // Bounds change every frame.
        NoFrustumCulling,
    ));
    commands.insert_resource(handles);
}

/// Clears the list and lays down the background: ambient glow and octave lanes
/// (skipped on the pure black background, which must stay #000000 for keying).
fn begin_frame(
    mut list: ResMut<DrawList>,
    layout: Res<Layout>,
    palette: Res<Palette>,
    settings: Res<Settings>,
) {
    list.0.clear();
    if settings.background != Background::Theme {
        return;
    }
    // With the keyboard hidden, the background runs to the bottom edge: no dark band under the line.
    let floor = if layout.keyboard {
        layout.hit_y
    } else {
        layout.bottom_y
    };
    let height = layout.top_y - floor;
    list.beam_strip(
        Vec2::new(0.0, floor),
        layout.width * 1.6,
        height * 0.45,
        |x| {
            glow(
                palette.accent_at(layout.keyboard_t(x)),
                0.05 * settings.glow,
            )
        },
    );
    for key in FIRST_KEY..=LAST_KEY {
        // Lines on the left edge of every C (and fainter on every F).
        let strength = match key % 12 {
            0 => 0.035,
            5 => 0.015,
            _ => continue,
        };
        let x = layout.key_x[key as usize] - layout.white_w / 2.0;
        list.rect(
            Vec2::new(x, floor + height / 2.0),
            Vec2::new(1.0, height),
            glow(palette.accent_key(key), strength),
        );
    }
}

/// Updates the shader parameters (relief, halo, line and top positions) when they change.
fn sync_material(
    settings: Res<Settings>,
    layout: Res<Layout>,
    handles: Res<DrawHandles>,
    mut materials: ResMut<Assets<DrawMaterial>>,
) {
    if !settings.is_changed() && !layout.is_changed() {
        return;
    }
    let params = Vec4::new(settings.relief, settings.halo, layout.hit_y, layout.top_y);
    if let Some(mut material) = materials.get_mut(&handles.material)
        && material.params != params
    {
        material.params = params;
    }
}

/// Rewrites the mesh from this frame's [`DrawList`].
fn build_mesh(list: Res<DrawList>, handles: Res<DrawHandles>, mut meshes: ResMut<Assets<Mesh>>) {
    if let Some(mut mesh) = meshes.get_mut(&handles.mesh) {
        // An empty buffer cannot be uploaded: draw one invisible quad instead.
        let quads = if list.0.is_empty() {
            &[EMPTY][..]
        } else {
            &list.0[..]
        };
        fill_mesh(&mut mesh, quads);
    }
}

/// An invisible quad, drawn when the list is empty (an empty buffer cannot be uploaded).
const EMPTY: Quad = Quad {
    center: Vec2::ZERO,
    extent: Vec2::ZERO,
    offset: Vec2::ZERO,
    half: Vec2::ZERO,
    param: 0.0,
    kind: KIND_RECT,
    left: LinearRgba::NONE,
    right: LinearRgba::NONE,
};

/// Corners of a quad, in units of its half size.
const CORNERS: [Vec2; 4] = [
    Vec2::new(-1.0, -1.0),
    Vec2::new(1.0, -1.0),
    Vec2::new(1.0, 1.0),
    Vec2::new(-1.0, 1.0),
];

/// Rewrites the mesh buffers in place.
fn fill_mesh(mesh: &mut Mesh, quads: &[Quad]) {
    if let Some(VertexAttributeValues::Float32x3(pos)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        pos.clear();
        for q in quads {
            pos.extend(CORNERS.map(|c| (q.center + c * q.extent).extend(0.0).to_array()));
        }
    }
    // Offset from the primitive's center (not the quad's), so slices of a strip line up.
    if let Some(VertexAttributeValues::Float32x2(local)) = mesh.attribute_mut(Mesh::ATTRIBUTE_UV_0)
    {
        local.clear();
        for q in quads {
            local.extend(CORNERS.map(|c| (q.offset + c * q.extent).to_array()));
        }
    }
    if let Some(VertexAttributeValues::Float32x4(shape)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_TANGENT)
    {
        shape.clear();
        for q in quads {
            shape.extend([[q.half.x, q.half.y, q.param, q.kind]; 4]);
        }
    }
    if let Some(VertexAttributeValues::Float32x4(color)) = mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
    {
        color.clear();
        for q in quads {
            color.extend(CORNERS.map(|c| {
                let side = if c.x < 0.0 { q.left } else { q.right };
                side.to_f32_array()
            }));
        }
    }
    if let Some(Indices::U32(indices)) = mesh.indices_mut() {
        indices.clear();
        for i in 0..quads.len() as u32 {
            let b = i * 4;
            indices.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
        }
    }
}
