//! Workaround for window resizing on DX12 (used when Vulkan is unavailable or WGPU_BACKEND=dx12).
//!
//! Camera views keep last frame's `ViewTarget` (which holds a view of the window's swapchain
//! image) in the render world until `prepare_view_targets` replaces it, i.e. *after* the surface
//! is reconfigured. On DX12 that leftover reference makes `ResizeBuffers` fail ("window is in
//! use") and Bevy quits. Vulkan tolerates it. Dropping those views right before the surface is
//! reconfigured fixes resizing, maximizing and fullscreen toggles.

use bevy::prelude::*;
use bevy::render::view::{ExtractedWindows, ViewTarget, ViewTargetAttachments, create_surfaces};
use bevy::render::{Render, RenderApp};

/// Adds [`release_window_views`] before the surfaces are reconfigured.
pub struct ResizeFixPlugin;

impl Plugin for ResizeFixPlugin {
    fn build(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(Render, release_window_views.before(create_surfaces));
        }
    }
}

/// On frames where a window is resized, drops every `ViewTarget` (and the attachments cache) so no
/// reference to the old swapchain images remains when DX12 resizes them.
fn release_window_views(world: &mut World) {
    let reconfiguring = world
        .resource::<ExtractedWindows>()
        .windows
        .values()
        .any(|w| w.size_changed || w.present_mode_changed);
    if !reconfiguring {
        return;
    }
    // Only runs on resize, so collecting the entities here is fine.
    let views: Vec<Entity> = world
        .query_filtered::<Entity, With<ViewTarget>>()
        .iter(world)
        .collect();
    for view in views {
        world.entity_mut(view).remove::<ViewTarget>();
    }
    world.resource_mut::<ViewTargetAttachments>().clear();
}
