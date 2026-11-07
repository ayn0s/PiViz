//! Bloom settings, the animated flame line, per-key glows and particles.
//!
//! Particles use a fixed pool of sprites recycled as a ring buffer: no allocation per frame.

use crate::colors::Palette;
use crate::compose::MainCamera;
use crate::draw::DrawList;
use crate::keyboard::Layout;
use crate::settings::{Background, Settings, glow};
use crate::shared::{FIRST_KEY, Frame, KeyHit, KeyLights, LAST_KEY};
use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::tonemapping::DebandDither;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Size of the particle pool; when it is full, the oldest particle is replaced.
const MAX_PARTICLES: usize = 6000;
/// Z of the key glows: above the keys.
const Z_GLOW: f32 = 13.0;
/// Z of the particles: above everything else in the scene.
const Z_PARTICLES: f32 = 14.0;
/// Height of the flame strip running along the whole keyboard.
const BASE_FLAME_HEIGHT: f32 = 45.0;
/// Extra height of the flame above a fully lit key.
const KEY_FLAME_HEIGHT: f32 = 75.0;
/// Seconds for a key flame to grow, and to die down after release.
const FLAME_RISE: f32 = 0.18;
/// Seconds for a key's flame to die down after release.
const FLAME_FALL: f32 = 0.6;

/// Builds an sRGB texture from a function of normalized pixel-center coordinates returning linear-ish RGBA in 0..1.
pub fn make_texture(width: u32, height: u32, f: impl Fn(f32, f32) -> [f32; 4]) -> Image {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let v = (y as f32 + 0.5) / height as f32;
            data.extend(f(u, v).map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

#[derive(Component)]
/// The soft glow sprite of a key, with its MIDI note number.
struct KeyGlow(u8);

#[derive(Component)]
/// A particle sprite, bound to `Particles::states[index]`.
struct Particle(usize);

#[derive(Clone, Copy, Default)]
/// Simulation state of one particle.
struct ParticleState {
    /// Position (world units).
    pos: Vec2,
    /// Velocity, in pixels per second.
    vel: Vec2,
    /// Seconds left; dead at 0.
    life: f32,
    /// Lifetime at birth, to compute the fade.
    max_life: f32,
    /// Size at birth, in pixels.
    size: f32,
    /// HDR color at birth.
    color: LinearRgba,
    /// Vertical acceleration: negative for falling sparks, positive for rising embers.
    lift: f32,
    /// Side-to-side drift amplitude (embers sway, sparks don't).
    sway: f32,
    /// Phase of the sway and flicker, so particles do not move in step.
    phase: f32,
    /// Sparks are stretched along their velocity into short streaks.
    streak: bool,
}

#[derive(Resource)]
/// The particle pool and per-key effect levels.
struct Particles {
    /// Every particle, used as a ring buffer.
    states: Vec<ParticleState>,
    /// Slot the next particle overwrites.
    next: usize,
    /// State of the xorshift random generator.
    rng: u64,
    /// Fractional embers owed per held key.
    ember_debt: [f32; 128],
    /// Glow level per key, decays after release.
    glow: [f32; 128],
    /// Smoothed flame level per key, eased in and out so flames never pop.
    flame: [f32; 128],
}

impl Particles {
    /// A random number in 0–1 (xorshift64*: fast, good enough for visual noise).
    fn rand(&mut self) -> f32 {
        // xorshift64*: good enough for visual noise.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A random number in `lo..hi`.
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rand()
    }

    /// Writes a particle in the next slot of the ring.
    fn emit(&mut self, p: ParticleState) {
        self.states[self.next] = p;
        self.next = (self.next + 1) % MAX_PARTICLES;
    }

    /// A fast spark thrown out of the impact, falling back under gravity.
    fn spark(&mut self, origin: Vec2, color: LinearRgba, strength: f32) {
        let angle = self.range(-1.2, 1.2);
        let speed = self.range(180.0, 560.0) * (0.6 + 0.7 * strength);
        let life = self.range(0.5, 1.2);
        let size = self.range(2.5, 5.0);
        let tint = self.range(0.0, 0.6);
        self.emit(ParticleState {
            pos: origin,
            vel: Vec2::new(angle.sin() * speed, angle.cos() * speed),
            life,
            max_life: life,
            size,
            color: glow(color.mix(&LinearRgba::WHITE, tint), 7.0),
            lift: -480.0,
            sway: 0.0,
            phase: 0.0,
            streak: true,
        });
    }

    /// A large, slow ember that floats up and sways.
    fn ember(&mut self, origin: Vec2, color: LinearRgba, scale: f32) {
        let life = self.range(0.9, 2.0);
        let size = self.range(4.0, 9.0) * scale;
        let vel = Vec2::new(self.range(-40.0, 40.0), self.range(60.0, 220.0));
        let sway = self.range(20.0, 70.0);
        let phase = self.range(0.0, std::f32::consts::TAU);
        self.emit(ParticleState {
            pos: origin,
            vel,
            life,
            max_life: life,
            size,
            color: glow(color, 3.5),
            lift: 60.0,
            sway,
            phase,
            streak: false,
        });
    }
}

/// Flames, glows, particles and bloom settings.
pub struct EffectsPlugin;

impl Plugin for EffectsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Particles {
            states: vec![ParticleState::default(); MAX_PARTICLES],
            next: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
            ember_debt: [0.0; 128],
            glow: [0.0; 128],
            flame: [0.0; 128],
        })
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                sync_bloom,
                spawn_bursts,
                base_flame,
                update_glows,
                update_particles,
            )
                .chain()
                .in_set(Frame::Visuals),
        );
    }
}

/// Creates the soft round texture and spawns the hidden glow and particle sprites once.
fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let soft = images.add(make_texture(64, 64, |u, v| {
        let d = Vec2::new(u - 0.5, v - 0.5).length() * 2.0;
        let a = (1.0 - d).clamp(0.0, 1.0).powf(2.2);
        [1.0, 1.0, 1.0, a]
    }));
    for key in FIRST_KEY..=LAST_KEY {
        commands.spawn((
            KeyGlow(key),
            Sprite::from_image(soft.clone()),
            Transform::from_xyz(0.0, 0.0, Z_GLOW),
            Visibility::Hidden,
        ));
    }
    for i in 0..MAX_PARTICLES {
        commands.spawn((
            Particle(i),
            Sprite {
                image: soft.clone(),
                custom_size: Some(Vec2::ONE),
                ..default()
            },
            Transform::from_xyz(0.0, 0.0, Z_PARTICLES),
            Visibility::Hidden,
        ));
    }
}

/// Applies the bloom setting to the scene camera, and turns dithering off except on the Theme
/// background, so black and transparent stay exactly zero.
fn sync_bloom(
    settings: Res<Settings>,
    camera: Single<(&mut Bloom, &mut DebandDither), With<MainCamera>>,
) {
    if !settings.is_changed() {
        return;
    }
    let (mut bloom, mut dither) = camera.into_inner();
    if bloom.intensity != settings.bloom {
        bloom.intensity = settings.bloom;
    }
    // Dithering adds faint noise to black; keyed or transparent backgrounds must stay exactly 0.
    let wanted = if settings.background == Background::Theme {
        DebandDither::Enabled
    } else {
        DebandDither::Disabled
    };
    dither.set_if_neq(wanted);
}

/// Sparks and a few big embers for each struck key.
fn spawn_bursts(
    mut hits: MessageReader<KeyHit>,
    layout: Res<Layout>,
    settings: Res<Settings>,
    mut particles: ResMut<Particles>,
) {
    for hit in hits.read() {
        let k = hit.key as usize;
        particles.glow[k] = 1.8;
        let strength = hit.velocity as f32 / 127.0;
        let sparks = ((10.0 + 30.0 * strength) * settings.sparks) as usize;
        let embers = ((2.0 + 5.0 * strength) * settings.sparks) as usize;
        let half = layout.note_w[k] / 2.0;
        for _ in 0..sparks {
            let origin = Vec2::new(layout.key_x[k] + particles.range(-half, half), layout.hit_y);
            particles.spark(origin, hit.color, strength);
        }
        for _ in 0..embers {
            let origin = Vec2::new(
                layout.key_x[k] + particles.range(-half, half),
                layout.hit_y + 4.0,
            );
            particles.ember(origin, hit.color, 1.0);
        }
    }
}

/// The flame strip along the whole keyboard, in the glow color (solid or gradient).
fn base_flame(
    time: Res<Time>,
    layout: Res<Layout>,
    palette: Res<Palette>,
    settings: Res<Settings>,
    mut draw: ResMut<DrawList>,
) {
    let (layout, palette) = (&*layout, &*palette);
    let color = |k: f32| move |x: f32| glow(palette.accent_at(layout.keyboard_t(x)), k);
    draw.flame_strip(
        Vec2::new(0.0, layout.hit_y - 1.0),
        Vec2::new(layout.span_w, BASE_FLAME_HEIGHT),
        time.elapsed_secs_wrapped(),
        // Soft ends instead of a hard cut at the sides.
        0.08,
        false,
        color(0.5 * settings.glow),
    );
    // Without the keyboard, a softer flame also hangs below the line down to the window edge.
    let below = layout.hit_y - layout.bottom_y;
    if !layout.keyboard && below > 1.0 {
        draw.flame_strip(
            Vec2::new(0.0, layout.hit_y + 1.0),
            Vec2::new(layout.span_w, below),
            time.elapsed_secs_wrapped(),
            0.08,
            true,
            color(0.35 * settings.glow),
        );
    }
}

/// Lit keys get a glow, a light beam, a taller local flame and a stream of embers.
#[allow(clippy::too_many_arguments)]
fn update_glows(
    time: Res<Time>,
    layout: Res<Layout>,
    lights: Res<KeyLights>,
    settings: Res<Settings>,
    mut particles: ResMut<Particles>,
    mut draw: ResMut<DrawList>,
    mut glows: Query<(&KeyGlow, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let dt = time.delta_secs();
    let now = time.elapsed_secs_wrapped();
    let beam_height = (layout.top_y - layout.hit_y) * 0.4;
    for (KeyGlow(key), mut sprite, mut tf, mut vis) in &mut glows {
        let k = *key as usize;
        let x = layout.key_x[k];
        let color = lights.color[k];

        // The flame eases in and out on its own timeline, independent of the impact flash.
        let flame = &mut particles.flame[k];
        *flame = if lights.lit[k] {
            (*flame + dt / FLAME_RISE).min(1.0)
        } else {
            (*flame - dt / FLAME_FALL).max(0.0)
        };
        let eased = *flame * *flame * (3.0 - 2.0 * *flame);
        if eased > 0.0 {
            let height = BASE_FLAME_HEIGHT + KEY_FLAME_HEIGHT * eased;
            draw.flame(
                Vec2::new(x, layout.hit_y - 1.0),
                Vec2::new(layout.white_w * 2.4, height),
                now,
                glow(color, 0.8 * settings.glow * eased),
                1.0,
            );
            let below = layout.hit_y - layout.bottom_y;
            if !layout.keyboard && below > 1.0 {
                draw.flame_down(
                    Vec2::new(x, layout.hit_y + 1.0),
                    Vec2::new(layout.white_w * 2.4, (height * 0.5).min(below)),
                    now,
                    glow(color, 0.5 * settings.glow * eased),
                    1.0,
                );
            }
        }

        let level = &mut particles.glow[k];
        *level = if lights.lit[k] {
            level.max(1.0) - (*level - 1.0).max(0.0) * (dt * 6.0).min(1.0)
        } else {
            (*level - dt * 2.5).max(0.0)
        };
        let level = *level;
        if level <= 0.0 {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        vis.set_if_neq(Visibility::Inherited);
        draw.beam(
            Vec2::new(x, layout.hit_y),
            layout.note_w[k] * 2.2,
            beam_height * level.min(1.2),
            glow(color, 0.25 * settings.glow * level),
        );
        let size = layout.white_w * (2.2 + level);
        sprite.custom_size = Some(Vec2::new(size, size * 0.8));
        tf.translation.x = x;
        tf.translation.y = layout.hit_y + 2.0;
        let mut halo = glow(color, 2.5 * settings.glow * level);
        halo.alpha = level.min(1.0) * 0.85;
        sprite.color = Color::from(halo);

        if lights.lit[k] && settings.sparks > 0.0 {
            particles.ember_debt[k] += dt * 14.0 * settings.sparks;
            while particles.ember_debt[k] >= 1.0 {
                particles.ember_debt[k] -= 1.0;
                let offset = particles.range(-0.5, 0.5) * layout.note_w[k];
                particles.ember(Vec2::new(x + offset, layout.hit_y), color, 0.7);
            }
        } else {
            particles.ember_debt[k] = 0.0;
        }
    }
}

/// Moves every live particle and updates its sprite (fade, flicker, size, streak direction); hides
/// dead ones.
fn update_particles(
    time: Res<Time>,
    mut particles: ResMut<Particles>,
    mut sprites: Query<(&Particle, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let dt = time.delta_secs();
    for (Particle(i), mut sprite, mut tf, mut vis) in &mut sprites {
        let p = &mut particles.states[*i];
        if p.life <= 0.0 {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        p.life -= dt;
        let age = p.max_life - p.life;
        p.vel.y += p.lift * dt;
        p.vel.x += (age * 4.0 + p.phase).sin() * p.sway * dt * 4.0;
        p.vel *= 1.0 - 1.1 * dt;
        p.pos += p.vel * dt;

        let fade = (p.life / p.max_life).clamp(0.0, 1.0);
        // Embers flicker; everything cools down as it fades.
        let flicker = if p.streak {
            1.0
        } else {
            0.75 + 0.25 * (age * 23.0 + p.phase).sin()
        };
        let mut color = glow(p.color, fade * flicker);
        color.alpha = fade.sqrt();
        sprite.color = Color::from(color);
        tf.translation = p.pos.extend(Z_PARTICLES);
        let size = p.size * (0.4 + 0.6 * fade);
        if p.streak {
            let speed = p.vel.length();
            tf.scale = Vec3::new(size * (1.0 + speed / 90.0), size * 0.8, 1.0);
            tf.rotation = Quat::from_rotation_z(p.vel.y.atan2(p.vel.x));
        } else {
            tf.scale = Vec3::splat(size);
            tf.rotation = Quat::IDENTITY;
        }
        vis.set_if_neq(Visibility::Inherited);
    }
}
