//! The overlay, injected into the game (`tomb_helper_ui.dll`, DirectX 12 through hudhook; built for
//! i686-pc-windows-msvc it's `tomb_helper_ui32.dll`, DirectX 11, for TR2013). Each frame it reads what the exe sent
//! (`ui_ipc_data`), shows its window, and draws in the world (clipped at the near plane, `projection`): the level's
//! collision triangles (faintly, under everything, without anti-aliasing, skipping back faces and ones off screen; the
//! exe has already left out those hidden behind others, as that's too slow to do here every frame), boxes and names for
//! objects, triggers, and Lara's collision. It reads the camera from the game itself each frame, at addresses the exe sends, as
//! the exe's copy lags and made everything judder as the camera turned. It ejects itself when the exe says to, or stops
//! updating. Opens a console only if `config.overlay.console`, and hooks DirectX through its vtables rather than by
//! patching its functions unless `config.overlay.hooks` is `"inline"`. Its logs (and panics) go to `tomb-helper.log`
//! next to it, as the exe's do (`logging`).

use imgui::*;
use hudhook::*;
// TR2013 is 32-bit and DirectX 11, the others 64-bit and DirectX 12
#[cfg(target_arch = "x86")]
use hudhook::hooks::dx11::ImguiDx11Hooks as GameHooks;
#[cfg(not(target_arch = "x86"))]
use hudhook::hooks::dx12::ImguiDx12Hooks as GameHooks;
use crate::ui_ipc_data::{hotkey, projection_from_lens, BoolSetting, BoxKind, HotkeyLabel, Capsule, F32Setting, ObjectBox, ObjectCategory, Probe, SharedMemoryManager, SurfaceKind, Triangle, TriggerCategory, U32Setting, UiData, UiSettings, REQUEST_RESTORE_POSITION, REQUEST_STORE_POSITION, REQUEST_TOGGLE_FLYING, TRIGGER_ACTIONS, COLLISION_KINDS};
use crate::position::Position;
use crate::projection::{box_distance, drawn_triangle, face_on_screen, fade, game_matrix, mesh_reach, Projector, MIN_OPACITY};
use crate::tracing::{error, info, warn};

// Shared with the exe, which uses the other half of them
#[allow(dead_code)]
mod logging;
#[allow(dead_code)]
mod ui_ipc_data;
#[allow(dead_code)]
mod position;
#[allow(dead_code)]
mod projection;

use nalgebra::{Matrix4, Vector3, Vector4};
use std::mem::{size_of, MaybeUninit};
use winapi::um::memoryapi::ReadProcessMemory;
use winapi::um::processthreadsapi::GetCurrentProcess;

// Lara's collision is moved along with her by how far she's gone since the exe read it, unless that's further than this
// (she's been moved, or reloaded)
const MAX_LARA_MOVE: f32 = 1000.0;
// How long without an update from the exe before the overlay decides it's gone and ejects itself
const EXE_GONE: std::time::Duration = std::time::Duration::from_secs(5);

// Lines per circle when drawing capsules
const CAPSULE_SEGMENTS: usize = 16;

const MODEL_COLOUR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const ESTIMATED_COLOUR: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
const LARA_COLOUR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
const INTERACTABLE_COLOUR: [f32; 4] = [0.3, 1.0, 0.8, 1.0];
const NPC_COLOUR: [f32; 4] = [1.0, 0.3, 0.2, 1.0];
// Triggers by category, and what they're called in the overlay's window
fn trigger_style(category: TriggerCategory) -> ([f32; 4], &'static str) {
    match category {
        TriggerCategory::Checkpoint => ([0.3, 1.0, 0.3, 1.0], "Checkpoints"),
        TriggerCategory::Death => ([1.0, 0.2, 0.2, 1.0], "Death"),
        TriggerCategory::Streaming => ([0.4, 0.7, 1.0, 1.0], "Level streaming"),
        TriggerCategory::Combat => ([1.0, 0.6, 0.1, 1.0], "Combat & AI"),
        TriggerCategory::Unknown => ([1.0, 0.45, 0.7, 1.0], "Unknown"),
        TriggerCategory::Camera => ([0.7, 0.7, 0.7, 1.0], "Camera"),
        TriggerCategory::Effects => ([0.8, 0.6, 1.0, 1.0], "Effects & sound"),
        TriggerCategory::Hints => ([1.0, 1.0, 0.5, 1.0], "Hints"),
        TriggerCategory::Objects => ([0.6, 0.5, 0.35, 1.0], "Objects (bushes, pickups, ...)"),
    }
}
// For each of `TRIGGER_ACTIONS`, in order. Not white, which is for triggers Lara's in.
const TRIGGER_ACTION_COLOURS: [[f32; 4]; 28] = [
    [0.3, 1.0, 0.3, 1.0],    // checkpoint
    [1.0, 0.85, 0.2, 1.0],   // objective
    [0.8, 0.7, 0.3, 1.0],    // progress
    [1.0, 0.3, 0.9, 1.0],    // cutscene
    [0.2, 0.9, 0.9, 1.0],    // moveset
    [0.7, 0.7, 0.7, 1.0],    // camera
    [0.4, 0.65, 1.0, 1.0],   // streaming
    [0.6, 0.4, 1.0, 1.0],    // teleport
    [1.0, 0.55, 0.1, 1.0],   // combat
    [1.0, 0.4, 0.3, 1.0],    // spawn
    [0.7, 0.3, 0.25, 1.0],   // despawn
    [1.0, 0.15, 0.15, 1.0],  // damage
    [1.0, 0.6, 0.75, 1.0],   // dialogue
    [1.0, 1.0, 0.4, 1.0],    // tutorial
    [0.8, 0.8, 0.55, 1.0],   // look at
    [0.85, 0.6, 0.4, 1.0],   // NPC
    [0.5, 0.8, 0.45, 1.0],   // inventory
    [1.0, 0.7, 0.4, 1.0],    // campsite
    [1.0, 0.9, 0.55, 1.0],   // lighting
    [0.6, 0.8, 0.95, 1.0],   // weather
    [0.75, 0.6, 1.0, 1.0],   // audio
    [0.6, 1.0, 0.8, 1.0],    // UI
    [0.8, 0.5, 0.6, 1.0],    // rumble
    [0.55, 0.55, 0.75, 1.0], // saves state
    [0.95, 0.75, 0.55, 1.0], // instances
    [1.0, 0.45, 0.7, 1.0],   // something else
    [0.5, 0.5, 0.5, 1.0],    // no script
    [0.35, 0.35, 0.35, 1.0], // unchecked
];

const _: () = assert!(TRIGGER_ACTION_COLOURS.len() == TRIGGER_ACTIONS.len());

// A trigger's colour: by what it does, or by the category of its name
fn trigger_colour(by_action: bool, category: TriggerCategory, action: u32) -> [f32; 4] {
    match TRIGGER_ACTION_COLOURS.get(action as usize) {
        Some(colour) if by_action => *colour,
        _ => trigger_style(category).0,
    }
}

const PROBE_COLOUR: [f32; 4] = [0.0, 1.0, 1.0, 1.0];
// The cross where Lara's aiming, when the collision meshes are drawn round it, and its half size
const AIM_MARKER_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const AIM_MARKER_SIZE: f32 = 15.0;
// The outline of a trigger Lara's in
const INSIDE_TRIGGER_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
// Lara's movement capsule, and the shape it's changing to
const PROXY_COLOURS: [[f32; 4]; 2] = [COLLISION_COLOUR, [1.0, 0.0, 0.0, 1.0]];
// Her collision capsules (her own, and those attached to it while they're used), normally and while they're pushing
// her off something
const COLLISION_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const PUSHING_COLOUR: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
// Half size of the cross drawn where a probe hit
const PROBE_HIT_SIZE: f32 = 4.0;
// The level's collision, by which way it faces. Edges are drawn in these, and the triangles filled in fainter.
const FLOOR_COLOUR: [f32; 4] = [0.2, 1.0, 0.2, 0.5];
const WALL_COLOUR: [f32; 4] = [1.0, 0.55, 0.1, 0.5];
const CEILING_COLOUR: [f32; 4] = [0.3, 0.6, 1.0, 0.5];
// Range of the sliders for how far from Lara collision meshes are drawn
const MAX_MESH_RADIUS: f32 = 5000.0;
// Meshes Lara doesn't collide with, whichever way they face
const DETAILED_COLOUR: [f32; 4] = [0.85, 0.4, 1.0, 0.5];
const SURFACE_FILL_ALPHA: f32 = 0.12;
// Range of the slider for how quickly what's drawn fades with distance
const MAX_FADE_STRENGTH: f32 = 8.0;
// Reads the game's memory, which the overlay is part of, through ReadProcessMemory so a bad address fails instead of
// crashing the game
fn read_game<T: Copy>(address: usize) -> Option<T> {
    if address == 0 {
        return None;
    }
    let mut value = MaybeUninit::<T>::uninit();
    let mut read = 0;
    let ok = unsafe {
        ReadProcessMemory(GetCurrentProcess(), address as _, value.as_mut_ptr() as _, size_of::<T>(), &mut read)
    };
    if ok == 0 || read != size_of::<T>() {
        return None;
    }
    Some(unsafe { value.assume_init() })
}

// Faded by its distance from Lara over `object_radius`
// Lara's box is moved by `lara_moved`
fn draw_box(
    ui: &Ui,
    draw_list: &DrawListMut,
    projector: &Projector,
    ui_data: &UiData,
    settings: &UiSettings,
    object: &ObjectBox,
    lara_moved: Vector3<f32>,
) {
    let colour = match (object.category, object.kind) {
        (ObjectCategory::Interactable, _) => INTERACTABLE_COLOUR,
        (ObjectCategory::Npc, _) => NPC_COLOUR,
        (_, BoxKind::Model) => MODEL_COLOUR,
        (_, BoxKind::Estimated) => ESTIMATED_COLOUR,
        (_, BoxKind::Lara) => LARA_COLOUR,
    };
    let mut model = game_matrix(&object.transform);
    if object.kind == BoxKind::Lara {
        model = Matrix4::new_translation(&lara_moved) * model;
    }
    let (min, max) = (object.min, object.max);
    // Corner i uses max for each axis whose bit is set in i
    let local: Vec<Vector3<f32>> = (0..8)
        .map(|i| {
            Vector3::new(
                if i & 1 != 0 { max.x } else { min.x },
                if i & 2 != 0 { max.y } else { min.y },
                if i & 4 != 0 { max.z } else { min.z },
            )
        })
        .collect();
    let world: Vec<Vector3<f32>> = local.iter().map(|&p| (model * p.push(1.0)).xyz()).collect();
    let opacity = fade(settings, box_distance(&ui_data.position.vector(), &world), ui_data.object_radius);
    if opacity < MIN_OPACITY {
        return;
    }
    let colour = with_opacity(colour, opacity);
    let corners: Vec<Vector4<f32>> = local.iter().map(|&point| projector.to_clip(&model, point)).collect();

    for a in 0..8 {
        for axis in [1, 2, 4] {
            if a & axis == 0 {
                if let Some((from, to)) = projector.project_line(corners[a], corners[a | axis]) {
                    draw_list.add_line(from, to, colour).thickness(1.5).build();
                }
            }
        }
    }

    // Centred above its highest point in the world (it can be tilted), like the trigger labels
    let (low, high) = world.iter().fold((world[0], world[0]), |(low, high), p| (low.inf(p), high.sup(p)));
    let top = Vector3::new((low.x + high.x) / 2.0, (low.y + high.y) / 2.0, high.z);
    if let Some(point) = projector.project(top) {
        draw_label(ui, draw_list, point, colour, object.name());
    }
}

// Text centred above a point on screen
fn draw_label(ui: &Ui, draw_list: &DrawListMut, point: [f32; 2], colour: [f32; 4], text: &str) {
    let size = ui.calc_text_size(text);
    draw_list.add_text([point[0] - size[0] / 2.0, point[1] - size[1]], colour, text);
}

// A convex polygon, on screen
fn fill_polygon(draw_list: &DrawListMut, polygon: &[[f32; 2]], colour: [f32; 4]) {
    for i in 2..polygon.len() {
        draw_list.add_triangle(polygon[0], polygon[i - 1], polygon[i], colour).filled(true).build();
    }
}

fn draw_world_line(draw_list: &DrawListMut, projector: &Projector, a: Vector3<f32>, b: Vector3<f32>, colour: [f32; 4]) {
    if let Some((from, to)) = projector.project_line(projector.world_to_clip(a), projector.world_to_clip(b)) {
        draw_list.add_line(from, to, colour).thickness(1.5).build();
    }
}

// Moved by `offset`
fn draw_capsule(draw_list: &DrawListMut, projector: &Projector, capsule: &Capsule, offset: Vector3<f32>, colour: [f32; 4]) {
    let a = capsule.a.vector() + offset;
    let b = capsule.b.vector() + offset;
    let radius = capsule.radius;
    let axis = (b - a).try_normalize(1e-4).unwrap_or_else(Vector3::z);
    let u = axis.cross(&if axis.x.abs() < 0.9 { Vector3::x() } else { Vector3::y() }).normalize();
    let v = axis.cross(&u);
    let angle = |i: usize| i as f32 * std::f32::consts::TAU / CAPSULE_SEGMENTS as f32;

    // A ring round each end of the cylinder, joined by four lines
    let ring = |centre: Vector3<f32>, t: f32| centre + (u * t.cos() + v * t.sin()) * radius;
    for i in 0..CAPSULE_SEGMENTS {
        for end in [a, b] {
            draw_world_line(draw_list, projector, ring(end, angle(i)), ring(end, angle(i + 1)), colour);
        }
    }
    for i in (0..CAPSULE_SEGMENTS).step_by(CAPSULE_SEGMENTS / 4) {
        draw_world_line(draw_list, projector, ring(a, angle(i)), ring(b, angle(i)), colour);
    }

    // Two half circles over each cap
    for (end, outwards) in [(a, -axis), (b, axis)] {
        for side in [u, v] {
            let cap = |t: f32| end + (side * t.cos() + outwards * t.sin()) * radius;
            for i in 0..CAPSULE_SEGMENTS / 2 {
                draw_world_line(draw_list, projector, cap(angle(i)), cap(angle(i + 1)), colour);
            }
        }
    }
}

// Moved by `offset`
fn draw_probe(draw_list: &DrawListMut, projector: &Projector, probe: &Probe, offset: Vector3<f32>, colour: [f32; 4]) {
    draw_world_line(draw_list, projector, probe.start.vector() + offset, probe.end.vector() + offset, colour);
    if probe.has_hit {
        let hit = probe.hit.vector() + offset;
        for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
            let offset = axis * PROBE_HIT_SIZE;
            draw_world_line(draw_list, projector, hit - offset, hit + offset, colour);
        }
    }
}

fn with_opacity(colour: [f32; 4], opacity: f32) -> [f32; 4] {
    [colour[0], colour[1], colour[2], colour[3] * opacity]
}

// `corners` are from `drawn_triangle`, and `opacity` scales the colours' own
fn draw_triangle(
    draw_list: &DrawListMut,
    projector: &Projector,
    triangle: &Triangle,
    corners: &[Vector4<f32>; 3],
    opacity: f32,
) {
    let mut colour = match triangle.kind {
        _ if triangle.detailed => DETAILED_COLOUR,
        SurfaceKind::Floor => FLOOR_COLOUR,
        SurfaceKind::Wall => WALL_COLOUR,
        SurfaceKind::Ceiling => CEILING_COLOUR,
    };
    colour[3] *= opacity;

    let fill = [colour[0], colour[1], colour[2], SURFACE_FILL_ALPHA * opacity];
    fill_polygon(draw_list, &projector.project_polygon(corners), fill);
    for i in 0..3 {
        if let Some((from, to)) = projector.project_line(corners[i], corners[(i + 1) % 3]) {
            draw_list.add_line(from, to, colour).build();
        }
    }
}

fn checkbox(ui: &Ui, label: &str, setting: &BoolSetting) {
    let mut value = setting.get();
    if ui.checkbox(label, &mut value) {
        setting.set(value);
    }
}

// A checkbox for each (bit, label, colour) of a setting that's a set of bits, filling the columns starting at
// `columns` (window x positions) after the first, a row at a time
fn bit_checkboxes(ui: &Ui, setting: &U32Setting, items: Vec<(u32, String, Option<[f32; 4]>)>, columns: &[f32]) {
    let mut bits = setting.get();
    for (i, (bit, label, colour)) in items.into_iter().enumerate() {
        let column = i % (columns.len() + 1);
        if column > 0 {
            ui.same_line_with_pos(columns[column - 1]);
        }
        let style = colour.map(|colour| ui.push_style_color(StyleColor::Text, colour));
        let mut value = bits & bit != 0;
        if ui.checkbox(label, &mut value) {
            bits ^= bit;
            setting.set(bits);
        }
        if let Some(style) = style {
            style.pop();
        }
    }
}

// For an f32 setting
fn slider(ui: &Ui, label: &str, min: f32, max: f32, format: &str, setting: &F32Setting) {
    let mut value = setting.get();
    if ui.slider_config(label, min, max).display_format(format).build(&mut value) {
        setting.set(value);
    }
}

struct TombHelperHud {
    ui_memory_manager: SharedMemoryManager,
    // Last complete copy, used when the exe was mid write
    ui_data: Box<UiData>,
    // Where the next copy is read to
    scratch: Box<UiData>,
    ejecting: bool,
    // When the exe last updated the shared memory, to notice it's gone (e.g. its window was closed, so it never said to
    // eject), as injecting again then does nothing while this copy is still loaded
    last_generation: u32,
    last_update: std::time::Instant,
    // How long the last frame's drawing took
    draw_milliseconds: f32,
}

impl TombHelperHud {
    fn new() -> Self {
        let ui_memory_manager = SharedMemoryManager::new(false);
        let mut ui_data = UiData::zeroed();
        ui_memory_manager.get().read(&mut ui_data);

        if ui_data.debug {
            if hudhook::alloc_console().is_ok() {
                hudhook::enable_console_colors();
            }
            else {
                warn!("console already exists for this process!");
            }
        }

        // To the console (if there is one) and the log file
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_thread_names(true))
            .with(tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_writer(logging::make_writer))
            .init();
        let game = std::env::current_exe().ok().and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()));
        info!("overlay loaded into {}", game.unwrap_or_default());

        Self {
            ui_memory_manager,
            ui_data,
            scratch: UiData::zeroed(),
            ejecting: false,
            last_generation: 0,
            last_update: std::time::Instant::now(),
            draw_milliseconds: 0.0,
        }
    }
}

impl ImguiRenderLoop for TombHelperHud {
    fn render(&mut self, ui: &mut Ui) {
        let started = std::time::Instant::now();
        if self.ui_memory_manager.get().try_read(&mut self.scratch) {
            std::mem::swap(&mut self.ui_data, &mut self.scratch);
        }
        self.eject_if_done();

        let ui_data = &*self.ui_data;
        let screen = ui.io().display_size;
        // Read again so changes show straight away, rather than once the exe has caught up
        let settings = &self.ui_memory_manager.get().settings;
        // For the exe to choose the triangles to draw
        settings.screen_width.set(screen[0]);
        settings.screen_height.set(screen[1]);
        if settings.show_window.get() {
            settings_window(ui, ui_data, settings, screen[0], self.draw_milliseconds);
        }
        if ui_data.camera_view_address != 0 && settings.overlay.get() {
            let projector = projector(ui_data, screen);
            let draw_list = ui.get_background_draw_list();
            let lara_moved = lara_moved(ui_data);
            draw_meshes(&draw_list, &projector, ui_data, settings, mesh_centre(ui_data, lara_moved));
            draw_objects(ui, &draw_list, &projector, ui_data, settings, lara_moved);
            if settings.triggers.get() {
                draw_triggers(ui, &draw_list, &projector, ui_data, settings);
            }
            draw_lara(&draw_list, &projector, ui_data, settings, lara_moved);
        }
        self.draw_milliseconds = started.elapsed().as_secs_f32() * 1000.0;
    }
}

impl TombHelperHud {
    // When the exe says to, or has stopped updating (e.g. its window was closed, so it never said to)
    fn eject_if_done(&mut self) {
        if self.ejecting {
            return;
        }
        // The exe updates every tick while the overlay's on, even while the game's paused
        let generation = self.ui_memory_manager.get().generation();
        if generation != self.last_generation {
            self.last_generation = generation;
            self.last_update = std::time::Instant::now();
        }
        if self.ui_data.should_exit {
            self.ejecting = true;
            error!("ejecting!");
            eject();
        } else if self.last_update.elapsed() > EXE_GONE {
            self.ejecting = true;
            error!("tomb-helper stopped updating, ejecting!");
            eject();
        }
    }
}

// The camera as it is now, read from the game itself as the exe's copy lags behind (which made everything judder as
// the camera turned)
fn projector(ui_data: &UiData, screen: [f32; 2]) -> Projector {
    let view = read_game(ui_data.camera_view_address as usize).unwrap_or(ui_data.camera_view);
    let projection = read_game(ui_data.camera_lens_address as usize)
        .map_or(ui_data.camera_projection, |lens| projection_from_lens(&lens, ui_data.lens_fields));
    Projector::new(&view, &projection, screen)
}

// How far Lara's gone since the exe read her position (and collision), read from the game itself like the camera, as the
// exe's ticks lag behind (more so while it rescans the level), which left her collision behind her as she ran
fn lara_moved(ui_data: &UiData) -> Vector3<f32> {
    let moved = read_game::<[f32; 3]>(ui_data.position_address as usize)
        .map_or(Vector3::zeros(), |[x, y, z]| Vector3::new(x, y, z) - ui_data.position.vector());
    // Also none if it's NaN
    if moved.norm() < MAX_LARA_MOVE { moved } else { Vector3::zeros() }
}

// Where the meshes are drawn round: Lara as she is now, or where she's aiming
fn mesh_centre(ui_data: &UiData, lara_moved: Vector3<f32>) -> Vector3<f32> {
    ui_data.mesh_centre.vector() + if ui_data.aiming { Vector3::zeros() } else { lara_moved }
}

// The level's collision, under everything else, furthest first, faded by distance from where it's drawn round (Lara or
// where she's aiming). Without anti-aliasing, which takes several times the vertices. The exe sends only those it saw
// drawn from the camera a moment ago, so this only checks that they still are.
fn draw_meshes(draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings, centre: Vector3<f32>) {
    let mesh_opacity = settings.mesh_opacity.get();
    let radii = [settings.coarse_radius.get(), settings.detailed_radius.get()];
    let reach = mesh_reach(settings);

    let flags = unsafe { &mut (*imgui::sys::igGetBackgroundDrawList()).Flags };
    let anti_aliased = *flags;
    *flags &= !((imgui::sys::ImDrawListFlags_AntiAliasedLines
        | imgui::sys::ImDrawListFlags_AntiAliasedLinesUseTex
        | imgui::sys::ImDrawListFlags_AntiAliasedFill) as i32);
    for triangle in ui_data.triangles().iter().rev() {
        if let Some((distance, corners)) = drawn_triangle(projector, triangle, &centre, &reach) {
            let opacity = mesh_opacity * fade(settings, distance, radii[triangle.detailed as usize]);
            draw_triangle(draw_list, projector, triangle, &corners, opacity);
        }
    }
    *flags = anti_aliased;
}

fn draw_objects(
    ui: &Ui,
    draw_list: &DrawListMut,
    projector: &Projector,
    ui_data: &UiData,
    settings: &UiSettings,
    lara_moved: Vector3<f32>,
) {
    let (interactables, npcs) = (settings.interactables.get(), settings.npcs.get());
    for object in ui_data.objects() {
        let shown = match object.category {
            ObjectCategory::Interactable => interactables,
            ObjectCategory::Npc => npcs,
            ObjectCategory::Named => true,
        };
        if shown {
            draw_box(ui, draw_list, projector, ui_data, settings, object, lara_moved);
        }
    }
}

// Trigger volumes filled in faintly, outlined (white if Lara's in them) and labelled above, each part faded by its
// distance from Lara over `object_radius`
fn draw_triggers(ui: &Ui, draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings) {
    let trigger_opacity = settings.trigger_opacity.get();
    let by_action = settings.triggers_by_action.get();
    let lara = ui_data.position.vector();
    let faded = |points: &[Vector3<f32>]| fade(settings, box_distance(&lara, points), ui_data.object_radius);
    if trigger_opacity > 0.0 {
        for face in ui_data.trigger_faces() {
            let world = face.corners.map(|c| c.vector());
            let opacity = trigger_opacity * faded(&world);
            if opacity < MIN_OPACITY {
                continue;
            }
            if let Some(corners) = face_on_screen(projector, world) {
                let fill = with_opacity(trigger_colour(by_action, face.category, face.action), opacity);
                fill_polygon(draw_list, &projector.project_polygon(&corners), fill);
            }
        }
    }
    for line in ui_data.trigger_lines() {
        let (a, b) = (line.a.vector(), line.b.vector());
        let opacity = faded(&[a, b]);
        if opacity < MIN_OPACITY {
            continue;
        }
        let colour = if line.inside { INSIDE_TRIGGER_COLOUR } else { trigger_colour(by_action, line.category, line.action) };
        draw_world_line(draw_list, projector, a, b, with_opacity(colour, opacity));
    }
    for label in ui_data.trigger_labels() {
        let position = label.position.vector();
        let opacity = faded(&[position]);
        if opacity < MIN_OPACITY {
            continue;
        }
        // Above the top of the volume
        if let Some(point) = projector.project(position) {
            let colour = with_opacity(trigger_colour(by_action, label.category, label.action), opacity);
            draw_label(ui, draw_list, point, colour, label.name());
        }
    }
}

// Where she's aiming (which the meshes are drawn round then), her collision (moved by `moved`), and a circle where the
// meshes are drawn round
fn draw_lara(draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings, moved: Vector3<f32>) {
    let centre = mesh_centre(ui_data, moved);
    if ui_data.aiming {
        for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
            let arm = axis * AIM_MARKER_SIZE;
            draw_world_line(draw_list, projector, centre - arm, centre + arm, AIM_MARKER_COLOUR);
        }
    }
    if settings.collision.get() && settings.lara_collision.get() {
        let opacity = |colour| with_opacity(colour, settings.mesh_opacity.get());
        for capsule in ui_data.capsules() {
            draw_capsule(draw_list, projector, capsule, moved, opacity(LARA_COLOUR));
        }
        for probe in ui_data.probes() {
            draw_probe(draw_list, projector, probe, moved, opacity(PROBE_COLOUR));
        }
        for (i, (proxy, colour)) in ui_data.proxies().iter().zip(PROXY_COLOURS).enumerate() {
            let colour = if i == 0 && ui_data.proxy_touching { PUSHING_COLOUR } else { colour };
            draw_capsule(draw_list, projector, proxy, moved, opacity(colour));
        }
        // The camera and weapon only collide while she aims
        if ui_data.attached_active {
            for (capsule, touching) in ui_data.attached().iter().zip(ui_data.attached_touching) {
                let colour = if touching { PUSHING_COLOUR } else { COLLISION_COLOUR };
                draw_capsule(draw_list, projector, capsule, moved, opacity(colour));
            }
        }
    }
    if let Some(point) = projector.project(centre) {
        draw_list.add_circle(point, 6.0, LARA_COLOUR).thickness(2.0).build();
    }
}

// The entry point `hudhook::hudhook!` makes, but choosing how the hooks are put in place (as the exe says) before
// they're made
/// # Safety
/// Only for Windows' loader, which calls every DLL's `DllMain`
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hmodule: hudhook::windows::Win32::Foundation::HINSTANCE,
    reason: u32,
    _: *mut std::ffi::c_void,
) -> i32 {
    if reason == hudhook::windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH {
        let hmodule_raw = hmodule.0 as usize;
        std::thread::spawn(move || {
            logging::log_panics();
            let hud = TombHelperHud::new();
            let method = if hud.ui_data.vtable_hooks { HookMethod::VTable } else { HookMethod::Inline };
            if let Err(e) = Hudhook::builder()
                .with_hook_method(method)
                .with::<GameHooks>(hud)
                .with_hmodule(hudhook::windows::Win32::Foundation::HINSTANCE(hmodule_raw as _))
                .build()
                .apply()
            {
                error!("Couldn't apply hooks: {e:?}");
                eject();
            }
        });
    }
    // A BOOL: 0 would have the loader unload the DLL again, before the thread above has started
    1
}

// F11 (`ToggleSettingsWindow`) hides it, leaving what's drawn in the world
fn settings_window(ui: &Ui, ui_data: &UiData, settings: &UiSettings, screen_width: f32, draw_milliseconds: f32) {
    ui.window(format!("{}###tomb-helper", keyed("tomb-helper", hotkey(ui_data, HotkeyLabel::Window))))
        .position_pivot([1., 0.])
        .position([screen_width, 0.], Condition::Once)
        .size([400., 650.], Condition::Once)
        .build(|| {
            if ui.collapsing_header("Lara", TreeNodeFlags::DEFAULT_OPEN) {
                position_text(ui, &ui_data.position);
            }
            if ui_data.position_tools && ui.collapsing_header("Position tools", TreeNodeFlags::DEFAULT_OPEN) {
                position_tools(ui, ui_data, settings);
            }
            if ui.collapsing_header("Overlay", TreeNodeFlags::DEFAULT_OPEN) {
                checkbox(ui, &keyed("On", hotkey(ui_data, HotkeyLabel::Overlay)), &settings.overlay);
                ui.text(format!("{} objects", ui_data.objects().len()));
                // Collision meshes fade out by the edge of their radius, objects and triggers by `object_radius`'s
                checkbox(ui, "Fade with distance from Lara", &settings.fade_with_distance);
                if settings.fade_with_distance.get() {
                    ui.indent();
                    slider(ui, "Fade strength", 1.0, MAX_FADE_STRENGTH, "%.1f", &settings.fade_strength);
                    ui.unindent();
                }
                for (label, control, setting) in [
                    ("Interactable objects", HotkeyLabel::Interactables, &settings.interactables),
                    ("Characters and animals", HotkeyLabel::Npcs, &settings.npcs),
                    ("Trigger volumes", HotkeyLabel::Triggers, &settings.triggers),
                ] {
                    checkbox(ui, &keyed(label, hotkey(ui_data, control)), setting);
                }
                if settings.triggers.get() {
                    ui.indent();
                    trigger_settings(ui, settings);
                    ui.unindent();
                }
                checkbox(ui, &keyed("Collision meshes", hotkey(ui_data, HotkeyLabel::Collision)), &settings.collision);
                if settings.collision.get() {
                    ui.indent();
                    ui.text(format!(
                        "{} of {} triangles ({:.1}ms to find, {:.1}ms to draw)",
                        ui_data.triangles().len(), ui_data.triangles_nearby, ui_data.triangle_milliseconds,
                        draw_milliseconds
                    ));
                    collision_settings(ui, ui_data, settings);
                    ui.unindent();
                }
            }
        });
}

// A control's label with the keys bound to it, e.g. "Flight mode (F5)"
fn keyed(label: &str, keys: &str) -> String {
    if keys.is_empty() {
        label.to_string()
    } else {
        format!("{} ({})", label, keys)
    }
}

fn position_text(ui: &Ui, position: &Position) {
    for (axis, value) in [("X", position.x), ("Y", position.y), ("Z", position.z)] {
        ui.text(format!("{} {:>12.2}", axis, value));
    }
}

// Flight mode and saving and restoring Lara's position, which the exe's position handler does when asked
fn position_tools(ui: &Ui, ui_data: &UiData, settings: &UiSettings) {
    let request = |bit: u32| {
        settings.requests.fetch_or(bit, std::sync::atomic::Ordering::SeqCst);
    };
    let mut flying = ui_data.flying;
    if ui.checkbox(keyed("Flight mode", hotkey(ui_data, HotkeyLabel::Flight)), &mut flying) {
        request(REQUEST_TOGGLE_FLYING);
    }
    if ui.is_item_hovered() {
        ui.tooltip_text("Holds Lara where she is; the movement hotkeys (W/A/S/D, Space and C by default) move her");
    }
    if ui.button(keyed("Save position", hotkey(ui_data, HotkeyLabel::SavePosition))) {
        request(REQUEST_STORE_POSITION);
    }
    if ui_data.has_saved_position {
        ui.same_line();
        if ui.button(keyed("Restore position", hotkey(ui_data, HotkeyLabel::RestorePosition))) {
            request(REQUEST_RESTORE_POSITION);
        }
        ui.text("Saved:");
        position_text(ui, &ui_data.saved_position);
    } else {
        ui.text_disabled("Nothing saved yet");
    }
}

// Which triggers are shown, by what they do or by name
fn trigger_settings(ui: &Ui, settings: &UiSettings) {
    let by_action = settings.triggers_by_action.get();
    ui.text("Show by:");
    ui.same_line();
    if ui.radio_button_bool("What they do", by_action) {
        settings.triggers_by_action.set(true);
    }
    ui.same_line();
    if ui.radio_button_bool("Name", !by_action) {
        settings.triggers_by_action.set(false);
    }
    if by_action {
        if ui.small_button("All") {
            settings.trigger_actions.set(u32::MAX);
        }
        ui.same_line();
        if ui.small_button("None") {
            settings.trigger_actions.set(0);
        }
        // Two columns, they're short
        let actions = TRIGGER_ACTIONS
            .iter()
            .enumerate()
            .map(|(i, action)| (1 << i, action.to_string(), Some(TRIGGER_ACTION_COLOURS[i])))
            .collect();
        bit_checkboxes(ui, &settings.trigger_actions, actions, &[190.0]);
    } else {
        let categories = TriggerCategory::ALL
            .iter()
            .map(|&category| {
                let (colour, label) = trigger_style(category);
                (category.bit(), label.to_string(), Some(colour))
            })
            .collect();
        bit_checkboxes(ui, &settings.trigger_categories, categories, &[]);
    }
    slider(ui, "Opacity##triggers", 0.0, 1.0, "%.2f", &settings.trigger_opacity);
}

// Lara's collision, and which of the level's collision meshes are drawn and how
fn collision_settings(ui: &Ui, ui_data: &UiData, settings: &UiSettings) {
    checkbox(ui, &keyed("Lara's collision", hotkey(ui_data, HotkeyLabel::LaraCollision)), &settings.lara_collision);
    checkbox(ui, &keyed("Movement collision (Lara's, 0x2)", hotkey(ui_data, HotkeyLabel::CoarseMeshes)), &settings.coarse_meshes);
    slider(ui, "Radius##coarse", 0.0, MAX_MESH_RADIUS, "%.0f", &settings.coarse_radius);
    checkbox(ui, &keyed("Other collision", hotkey(ui_data, HotkeyLabel::DetailedMeshes)), &settings.detailed_meshes);
    slider(ui, "Radius##detailed", 0.0, MAX_MESH_RADIUS, "%.0f", &settings.detailed_radius);
    if settings.detailed_meshes.get() {
        // Which kinds of collision (bits of the meshes' filters), and how many meshes in the level have each; what
        // they're for isn't known, besides 0x2 (Lara's movement)
        ui.indent();
        let kinds = (0..COLLISION_KINDS)
            .filter(|&bit| ui_data.mesh_kind_counts[bit] != 0)
            .map(|bit| (1 << bit, format!("{:X} ({})##kind", 1 << bit, ui_data.mesh_kind_counts[bit]), None))
            .collect();
        bit_checkboxes(ui, &settings.other_kinds, kinds, &[155.0, 280.0]);
        ui.unindent();
    }
    slider(ui, "Opacity", 0.0, 1.0, "%.2f", &settings.mesh_opacity);
    checkbox(ui, "Hide triangles behind others", &settings.cull_hidden);
    checkbox(ui, "Round where Lara's aiming, while she aims", &settings.follow_aim);
}
