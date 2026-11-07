//! Cameras and final composition.
//!
//! The scene camera (HDR, bloom, tone mapping) renders into an offscreen image. A second camera,
//! the only one drawing to the window, shows that image through `shaders/compose.wgsl` (which fades
//! the window border, builds the alpha channel for the transparent background, or blends it over
//! the webcam image), then draws egui on top, so the panel is neither bloomed nor faded.

use crate::keyboard::Layout;
use crate::persist::Session;
use crate::settings::{Background, Settings};
use crate::shared::Frame;
use crate::webcam::{self, Webcam};
use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Hdr, ImageRenderTarget, RenderTarget};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendState, Extent3d, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiGlobalSettings, PrimaryEguiContext};

/// `shaders/compose.wgsl`, embedded in the binary under this fixed id.
const COMPOSE_SHADER: Handle<Shader> = uuid_handle!("2f9d6c41-8b7e-4a35-b0c2-71e94d5a3f86");
/// Render layer of the composition quad, seen only by the composition camera.
const COMPOSE_LAYER: usize = 1;

/// The scene camera (bloom, tone mapping), rendering offscreen.
#[derive(Component)]
pub struct MainCamera;

#[derive(Component)]
/// The full-window quad drawn by the composition camera.
struct ComposeQuad;

#[derive(ShaderType, Clone, Copy, PartialEq, Default)]
/// Uniform of `compose.wgsl`; mirrored field by field by its `Params` struct.
struct ComposeParams {
    /// x: edge fade width, y: transparent background, zw: window size (logical px).
    settings: Vec4,
    /// Linear background color the border fades to.
    background: Vec4,
    /// x: camera background, y: camera brightness, z: the scene has a valid projection.
    camera: Vec4,
    /// Camera image within the window, in window uv: xy offset, zw size.
    camera_rect: Vec4,
    /// Camera uv -> scene uv homography, by rows.
    to_scene_x: Vec4,
    /// Second row of the camera → scene homography.
    to_scene_y: Vec4,
    /// Third row of the camera → scene homography (the perspective divisor).
    to_scene_w: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
/// Material of the composition quad.
struct ComposeMaterial {
    #[uniform(0)]
    /// Shader parameters, see [`ComposeParams`].
    params: ComposeParams,
    #[texture(1)]
    #[sampler(2)]
    /// The scene camera's output: bloomed, tone-mapped scene.
    scene: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    /// The latest webcam frame (owned by [`Webcam`]).
    camera: Handle<Image>,
}

impl Material2d for ComposeMaterial {
    fn fragment_shader() -> ShaderRef {
        COMPOSE_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    // The shader already outputs the final premultiplied pixel: write it as is.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(BlendState::REPLACE);
            }
        }
        Ok(())
    }
}

#[derive(Resource)]
/// Handles that systems need to find again.
struct ComposeHandles {
    /// The offscreen image the scene camera renders into.
    scene: Handle<Image>,
    /// The composition quad's material.
    material: Handle<ComposeMaterial>,
}

/// Sets up both cameras and the final composition.
pub struct ComposePlugin;

impl Plugin for ComposePlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            COMPOSE_SHADER,
            "shaders/compose.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(Material2dPlugin::<ComposeMaterial>::default())
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (fit_to_window, sync_material).chain().in_set(Frame::Draw),
            );
    }
}

#[allow(clippy::too_many_arguments)]
/// Creates the scene image, the scene camera (HDR, bloom, tone mapping), the composition camera
/// that also draws egui, and the full-window quad.
fn setup(
    mut commands: Commands,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ComposeMaterial>>,
    mut egui: ResMut<EguiGlobalSettings>,
    settings: Res<Settings>,
    webcam: Res<Webcam>,
) {
    let mut image = Image::new_target_texture(
        window.physical_width().max(1),
        window.physical_height().max(1),
        TextureFormat::Rgba16Float,
        None,
    );
    // GPU-only, and redrawn every frame: no CPU copy, nothing to carry over on resize.
    image.data = None;
    image.copy_on_resize = false;
    let scene = images.add(image);

    commands.spawn((
        MainCamera,
        Camera2d,
        Hdr,
        Tonemapping::TonyMcMapface,
        DebandDither::Enabled,
        Bloom {
            intensity: settings.bloom,
            ..Bloom::NATURAL
        },
        RenderTarget::Image(ImageRenderTarget {
            handle: scene.clone(),
            scale_factor: window.scale_factor(),
        }),
    ));

    // The only camera drawing to the window: the composed scene, then egui on top. One camera
    // means no textures shared between window cameras (a source of one-frame glitches), and the
    // panel is neither bloomed, tone-mapped nor faded.
    egui.auto_create_primary_context = false;
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        RenderLayers::layer(COMPOSE_LAYER),
    ));

    let material = materials.add(ComposeMaterial {
        params: ComposeParams::default(),
        scene: scene.clone(),
        camera: webcam.image.clone(),
    });
    commands.spawn((
        ComposeQuad,
        Mesh2d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial2d(material.clone()),
        Transform::from_scale(Vec3::new(window.width(), window.height(), 1.0)),
        RenderLayers::layer(COMPOSE_LAYER),
    ));
    commands.insert_resource(ComposeHandles { scene, material });
}

/// Keeps the offscreen scene at the window's physical size and the quad covering the window.
fn fit_to_window(
    window: Single<&Window, With<PrimaryWindow>>,
    handles: Res<ComposeHandles>,
    mut images: ResMut<Assets<Image>>,
    mut camera: Single<&mut RenderTarget, With<MainCamera>>,
    mut quad: Single<&mut Transform, With<ComposeQuad>>,
) {
    let (width, height) = (
        window.physical_width().max(1),
        window.physical_height().max(1),
    );
    let Some(image) = images.get(&handles.scene) else {
        return;
    };
    let size = image.texture_descriptor.size;
    let scale_factor = window.scale_factor();
    let target_ok = matches!(&**camera, RenderTarget::Image(t) if t.scale_factor == scale_factor);
    if size.width == width && size.height == height && target_ok {
        return;
    }
    if let Some(mut image) = images.get_mut(&handles.scene) {
        image.resize(Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        });
    }
    **camera = RenderTarget::Image(ImageRenderTarget {
        handle: handles.scene.clone(),
        scale_factor,
    });
    quad.scale = Vec3::new(window.width(), window.height(), 1.0);
}

#[allow(clippy::too_many_arguments)]
/// Recomputes the shader parameters each frame (edge fade, background, camera rectangle,
/// homography) and writes them only when they changed or the camera texture was replaced.
fn sync_material(
    settings: Res<Settings>,
    layout: Res<Layout>,
    clear: Res<ClearColor>,
    session: Res<Session>,
    webcam: Res<Webcam>,
    handles: Res<ComposeHandles>,
    mut materials: ResMut<Assets<ComposeMaterial>>,
    mut generation: Local<u64>,
) {
    let mut params = ComposeParams {
        settings: Vec4::new(
            layout.edge_fade,
            f32::from(settings.background == Background::Transparent),
            layout.width,
            layout.height,
        ),
        background: Vec4::from_array(LinearRgba::from(clear.0).to_f32_array()),
        ..default()
    };
    if settings.background == Background::Camera {
        let window = Vec2::new(layout.width, layout.height);
        let rect = webcam::fit_rect(window, webcam.size_or(window));
        let to_scene = webcam::camera_to_scene(&layout, &session.camera_points);
        params.camera = Vec4::new(
            1.0,
            settings.camera_brightness,
            f32::from(to_scene.is_some()),
            0.0,
        );
        params.camera_rect = Vec4::new(
            rect.min.x / window.x,
            rect.min.y / window.y,
            rect.width() / window.x,
            rect.height() / window.y,
        );
        // Mat3 is column-major: row i is (x_axis[i], y_axis[i], z_axis[i]).
        let h = to_scene.unwrap_or_default().transpose();
        params.to_scene_x = h.x_axis.extend(0.0);
        params.to_scene_y = h.y_axis.extend(0.0);
        params.to_scene_w = h.z_axis.extend(0.0);
    }
    // A replaced camera texture (new frame size) needs the material's bind group rebuilt.
    let new_texture = webcam.generation != *generation;
    *generation = webcam.generation;
    let Some(current) = materials.get(&handles.material) else {
        return;
    };
    if current.params == params && !new_texture {
        return;
    }
    if let Some(mut material) = materials.get_mut(&handles.material) {
        material.params = params;
    }
}
