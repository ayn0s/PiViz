//! Window icon (title bar, taskbar, Alt+Tab), decoded from `assets/icon.png` embedded in the binary.
//!
//! The executable's own icon on Windows is embedded by `build.rs`. Both files are rendered from
//! `assets/logo.svg`.

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::NonSendMarker;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WINIT_WINDOWS;
use winit::window::Icon;

/// `assets/icon.png`, embedded in the binary.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Sets the window icon once the OS window exists.
pub struct IconPlugin;

impl Plugin for IconPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, set_window_icon);
    }
}

/// Runs until the OS window exists, then sets its icon once.
fn set_window_icon(
    window: Single<Entity, With<PrimaryWindow>>,
    mut done: Local<bool>,
    // The winit windows live on the main thread.
    _main_thread: NonSendMarker,
) {
    if *done {
        return;
    }
    WINIT_WINDOWS.with_borrow(|windows| {
        let Some(winit_window) = windows.get_window(*window) else {
            return;
        };
        *done = true;
        match decode_icon() {
            Ok(icon) => {
                // On Windows the taskbar and Alt+Tab use a separate, larger icon.
                #[cfg(windows)]
                {
                    use winit::platform::windows::WindowExtWindows;
                    winit_window.set_taskbar_icon(Some(icon.clone()));
                }
                winit_window.set_window_icon(Some(icon));
            }
            Err(e) => warn!("cannot set the window icon: {e}"),
        }
    });
}

/// Decodes [`ICON_PNG`] into a winit icon.
fn decode_icon() -> Result<Icon, String> {
    let image = Image::from_buffer(
        ICON_PNG,
        ImageType::Extension("png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::Default,
        RenderAssetUsages::MAIN_WORLD,
    )
    .map_err(|e| e.to_string())?;
    let (width, height) = (image.width(), image.height());
    let rgba = image.data.ok_or("empty image")?;
    Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes() {
        decode_icon().unwrap();
    }
}
