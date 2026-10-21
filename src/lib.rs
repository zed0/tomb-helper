//! The overlay, injected into the game (`tomb_helper_ui.dll`, DirectX 12 through hudhook; built for
//! i686-pc-windows-msvc it's `tomb_helper_ui32.dll`, DirectX 11, for TR2013). Each frame it reads what the exe sent
//! (`ui_ipc_data`), shows its window, and draws in the world (clipped at the near plane): the level's collision
//! triangles (faintly, under everything, without anti-aliasing, skipping back faces, ones off screen, and optionally
//! ones behind others through a quarter resolution software ID buffer, `OcclusionBuffer`), boxes and names for objects,
//! triggers, and Lara's collision. It reads the camera from the game itself each frame, at addresses the exe sends, as
//! the exe's copy lags and made everything judder as the camera turned. It ejects itself when the exe says to, or stops
//! updating. Opens a console only if `config.overlay.console`.

use imgui::*;
use hudhook::*;
// TR2013 is 32-bit and DirectX 11, the others 64-bit and DirectX 12
#[cfg(target_arch = "x86")]
use hudhook::hooks::dx11::ImguiDx11Hooks as GameHooks;
#[cfg(not(target_arch = "x86"))]
use hudhook::hooks::dx12::ImguiDx12Hooks as GameHooks;
use crate::ui_ipc_data::{hotkey, projection_from_lens, BoolSetting, BoxKind, HotkeyLabel, Capsule, F32Setting, ObjectBox, ObjectCategory, Probe, SharedMemoryManager, SurfaceKind, Triangle, TriggerCategory, U32Setting, UiData, UiSettings, REQUEST_RESTORE_POSITION, REQUEST_STORE_POSITION, REQUEST_TOGGLE_FLYING, TRIGGER_ACTIONS};
use crate::position::Position;
use crate::tracing::{error, warn};

// Shared with the exe, which uses the other half of them
#[allow(dead_code)]
mod ui_ipc_data;
#[allow(dead_code)]
mod position;

use nalgebra::{Matrix4, Vector3, Vector4};
use std::mem::{size_of, MaybeUninit};
use winapi::um::memoryapi::ReadProcessMemory;
use winapi::um::processthreadsapi::GetCurrentProcess;

// How long without an update from the exe before the overlay decides it's gone and ejects itself
const EXE_GONE: std::time::Duration = std::time::Duration::from_secs(5);

// How many of a list are in use, given the count the exe sent, which may be mid write
fn count(count: u32, len: usize) -> usize {
    (count as usize).min(len)
}

// Points closer to the camera plane than this (in clip space w) are clipped off
const NEAR_CLIP_W: f32 = 1.0;
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
// Range of the slider for how quickly meshes fade with distance
const MAX_FADE_STRENGTH: f32 = 8.0;
// Triangles faded more than this aren't drawn
const MIN_OPACITY: f32 = 0.005;
// When culling hidden triangles, they're rasterised into a buffer this many times smaller than the screen, keeping
// the nearest at each pixel, and only those nearest somewhere are drawn. Smaller triangles can be missed.
const OCCLUSION_SCALE: f32 = 4.0;

// The game stores matrices row-major for row vectors (`v * M`). Reading the same floats column-major
// gives the transposed matrix, which is the equivalent for nalgebra's column vectors (`M * v`).
fn game_matrix(elements: &[f32; 16]) -> Matrix4<f32> {
    Matrix4::from_column_slice(elements)
}

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

// The part of the convex polygon (in clip space) in front of the near plane
fn clip_polygon(corners: &[Vector4<f32>]) -> Vec<Vector4<f32>> {
    let mut clipped = vec![];
    for (i, &a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        if a.w >= NEAR_CLIP_W {
            clipped.push(a);
        }
        if (a.w >= NEAR_CLIP_W) != (b.w >= NEAR_CLIP_W) {
            let t = (a.w - NEAR_CLIP_W) / (a.w - b.w);
            clipped.push(a + (b - a) * t);
        }
    }
    clipped
}

// Keeps which triangle is nearest the camera at each pixel, at a lower resolution than the screen
#[derive(Default)]
struct OcclusionBuffer {
    width: usize,
    height: usize,
    // 1 / w, which is linear across the screen, so bigger is nearer
    depth: Vec<f32>,
    ids: Vec<u32>,
}

impl OcclusionBuffer {
    fn reset(&mut self, screen_width: f32, screen_height: f32) {
        self.width = (screen_width / OCCLUSION_SCALE).ceil().max(1.0) as usize;
        self.height = (screen_height / OCCLUSION_SCALE).ceil().max(1.0) as usize;
        self.depth.clear();
        self.depth.resize(self.width * self.height, 0.0);
        self.ids.clear();
        self.ids.resize(self.width * self.height, u32::MAX);
    }

    // `corners` are in clip space
    fn add(&mut self, id: u32, projector: &Projector, corners: &[Vector4<f32>; 3]) {
        let points: Vec<[f32; 3]> = clip_polygon(corners)
            .into_iter()
            .map(|c| {
                let [x, y] = projector.to_screen(c);
                [x / OCCLUSION_SCALE, y / OCCLUSION_SCALE, 1.0 / c.w]
            })
            .collect();
        for i in 2..points.len() {
            self.rasterise(id, [points[0], points[i - 1], points[i]]);
        }
    }

    fn rasterise(&mut self, id: u32, p: [[f32; 3]; 3]) {
        let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-6 {
            return;
        }
        let min_x = p.iter().map(|c| c[0]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
        let max_x = p.iter().map(|c| c[0]).fold(f32::MIN, f32::max).ceil().min(self.width as f32 - 1.0) as i64;
        let min_y = p.iter().map(|c| c[1]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
        let max_y = p.iter().map(|c| c[1]).fold(f32::MIN, f32::max).ceil().min(self.height as f32 - 1.0) as i64;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Barycentric weights, all positive inside whichever way it's wound
                let w0 = edge(p[1], p[2], px, py) / area;
                let w1 = edge(p[2], p[0], px, py) / area;
                let w2 = edge(p[0], p[1], px, py) / area;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let depth = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
                let i = y as usize * self.width + x as usize;
                if depth > self.depth[i] {
                    self.depth[i] = depth;
                    self.ids[i] = id;
                }
            }
        }
    }

    // Which of the first `count` ids are nearest at some pixel
    fn visible(&self, count: usize) -> Vec<bool> {
        let mut visible = vec![false; count];
        for &id in &self.ids {
            if let Some(v) = visible.get_mut(id as usize) {
                *v = true;
            }
        }
        visible
    }
}

struct Projector {
    view_projection: Matrix4<f32>,
    camera_position: Vector3<f32>,
    screen_width: f32,
    screen_height: f32,
}

impl Projector {
    fn to_clip(&self, model: &Matrix4<f32>, point: Vector3<f32>) -> Vector4<f32> {
        self.view_projection * model * point.push(1.0)
    }

    fn world_to_clip(&self, point: Vector3<f32>) -> Vector4<f32> {
        self.view_projection * point.push(1.0)
    }

    fn to_screen(&self, clip: Vector4<f32>) -> [f32; 2] {
        let ndc = clip.xy() / clip.w;
        // The game's view space has y pointing down the screen, so there's no flip here
        [
            (ndc.x + 1.0) * 0.5 * self.screen_width,
            (ndc.y + 1.0) * 0.5 * self.screen_height,
        ]
    }

    fn project(&self, point: Vector3<f32>) -> Option<[f32; 2]> {
        let clip = self.world_to_clip(point);
        if clip.w < NEAR_CLIP_W {
            return None;
        }
        Some(self.to_screen(clip))
    }

    // Clips the convex polygon against the near plane, returns its corners on screen (none if it's entirely behind
    // the camera)
    fn project_polygon(&self, corners: &[Vector4<f32>]) -> Vec<[f32; 2]> {
        clip_polygon(corners).into_iter().map(|c| self.to_screen(c)).collect()
    }

    // Clips the line against the near plane, returns None if it's entirely behind the camera
    fn project_line(&self, a: Vector4<f32>, b: Vector4<f32>) -> Option<([f32; 2], [f32; 2])> {
        if a.w < NEAR_CLIP_W && b.w < NEAR_CLIP_W {
            return None;
        }
        let clip = |inside: Vector4<f32>, outside: Vector4<f32>| {
            let t = (inside.w - NEAR_CLIP_W) / (inside.w - outside.w);
            inside + (outside - inside) * t
        };
        let a2 = if a.w < NEAR_CLIP_W { clip(b, a) } else { a };
        let b2 = if b.w < NEAR_CLIP_W { clip(a, b) } else { b };
        Some((self.to_screen(a2), self.to_screen(b2)))
    }
}

fn draw_box(ui: &Ui, draw_list: &DrawListMut, projector: &Projector, object: &ObjectBox) {
    let colour = match (object.category, object.kind) {
        (ObjectCategory::Interactable, _) => INTERACTABLE_COLOUR,
        (ObjectCategory::Npc, _) => NPC_COLOUR,
        (_, BoxKind::Model) => MODEL_COLOUR,
        (_, BoxKind::Estimated) => ESTIMATED_COLOUR,
        (_, BoxKind::Lara) => LARA_COLOUR,
    };
    let model = game_matrix(&object.transform);
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
    let world: Vec<Vector3<f32>> = local.iter().map(|&p| (model * p.push(1.0)).xyz()).collect();
    let (low, high) = world.iter().fold((world[0], world[0]), |(low, high), p| (low.inf(p), high.sup(p)));
    let top = Vector3::new((low.x + high.x) / 2.0, (low.y + high.y) / 2.0, high.z);
    if let Some(point) = projector.project(top) {
        let size = ui.calc_text_size(object.name());
        draw_list.add_text([point[0] - size[0] / 2.0, point[1] - size[1]], colour, object.name());
    }
}

fn draw_world_line(draw_list: &DrawListMut, projector: &Projector, a: Vector3<f32>, b: Vector3<f32>, colour: [f32; 4]) {
    if let Some((from, to)) = projector.project_line(projector.world_to_clip(a), projector.world_to_clip(b)) {
        draw_list.add_line(from, to, colour).thickness(1.5).build();
    }
}

fn draw_capsule(draw_list: &DrawListMut, projector: &Projector, capsule: &Capsule, colour: [f32; 4]) {
    let a = capsule.a.vector();
    let b = capsule.b.vector();
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

fn draw_probe(draw_list: &DrawListMut, projector: &Projector, probe: &Probe, colour: [f32; 4]) {
    draw_world_line(draw_list, projector, probe.start.vector(), probe.end.vector(), colour);
    if probe.has_hit {
        let hit = probe.hit.vector();
        for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
            let offset = axis * PROBE_HIT_SIZE;
            draw_world_line(draw_list, projector, hit - offset, hit + offset, colour);
        }
    }
}

fn with_opacity(colour: [f32; 4], opacity: f32) -> [f32; 4] {
    [colour[0], colour[1], colour[2], colour[3] * opacity]
}

// How far the point is from the nearest part of the box round the triangle, like the exe's choice of triangles
fn triangle_distance(point: &Vector3<f32>, triangle: &Triangle) -> f32 {
    let corners = triangle.corners.map(|c| c.vector());
    let min = corners[0].inf(&corners[1]).inf(&corners[2]);
    let max = corners[0].sup(&corners[1]).sup(&corners[2]);
    (min - point).sup(&(point - max)).sup(&Vector3::zeros()).norm()
}

// The triangle's corners in clip space, unless it's seen from behind or can't be on screen
fn triangle_on_screen(projector: &Projector, triangle: &Triangle) -> Option<[Vector4<f32>; 3]> {
    let world = triangle.corners.map(|c| c.vector());
    // Triangles are wound anticlockwise seen from the side they face
    let normal = (world[1] - world[0]).cross(&(world[2] - world[0]));
    if normal.dot(&(projector.camera_position - world[0])) <= 0.0 {
        return None;
    }
    face_on_screen(projector, world)
}

// The corners in clip space, unless they can't be on screen
fn face_on_screen(projector: &Projector, world: [Vector3<f32>; 3]) -> Option<[Vector4<f32>; 3]> {
    let corners = world.map(|c| projector.world_to_clip(c));
    // All off the same side of the screen, or behind the camera
    let outside = |test: fn(&Vector4<f32>) -> bool| corners.iter().all(test);
    if outside(|c| c.x < -c.w)
        || outside(|c| c.x > c.w)
        || outside(|c| c.y < -c.w)
        || outside(|c| c.y > c.w)
        || outside(|c| c.w < NEAR_CLIP_W)
    {
        return None;
    }
    Some(corners)
}

// `corners` are from `triangle_on_screen`, and `opacity` scales the colours' own
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
    let polygon = projector.project_polygon(corners);
    for i in 2..polygon.len() {
        draw_list.add_triangle(polygon[0], polygon[i - 1], polygon[i], fill).filled(true).build();
    }
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
    // Reused between frames
    occlusion: OcclusionBuffer,
}

impl TombHelperHud {
    fn new() -> Self {
        let ui_memory_manager = SharedMemoryManager::new(false);
        let mut ui_data = UiData::zeroed();
        ui_memory_manager.get().read(&mut ui_data);

        if ui_data.debug {
            if let Ok(_) = hudhook::alloc_console() {
                hudhook::enable_console_colors();
            }
            else {
                warn!("console already exists for this process!");
            }
        }

        tracing_subscriber::fmt()
            .with_thread_ids(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_names(true)
            .init();

        Self {
            ui_memory_manager,
            ui_data,
            scratch: UiData::zeroed(),
            ejecting: false,
            last_generation: 0,
            last_update: std::time::Instant::now(),
            draw_milliseconds: 0.0,
            occlusion: OcclusionBuffer::default(),
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
        if settings.show_window.get() {
            let object_count = count(ui_data.object_count, ui_data.objects.len());
            let triangle_count = count(ui_data.triangle_count, ui_data.triangles.len());
            self.settings_window(ui, ui_data, settings, screen[0], object_count, triangle_count);
        }
        if ui_data.camera_view_address != 0 && settings.overlay.get() {
            let projector = projector(ui_data, screen);
            let draw_list = ui.get_background_draw_list();
            draw_meshes(&draw_list, &projector, ui_data, settings, &mut self.occlusion);
            draw_objects(ui, &draw_list, &projector, ui_data, settings);
            if settings.triggers.get() {
                draw_triggers(ui, &draw_list, &projector, ui_data, settings);
            }
            draw_lara(&draw_list, &projector, ui_data, settings);
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
    let view = game_matrix(&read_game(ui_data.camera_view_address as usize).unwrap_or(ui_data.camera_view));
    let projection = read_game(ui_data.camera_lens_address as usize)
        .map_or(ui_data.camera_projection, |lens| projection_from_lens(&lens, ui_data.lens_fields));
    Projector {
        view_projection: game_matrix(&projection) * view,
        camera_position: view.try_inverse().map_or(Vector3::zeros(), |m| m.column(3).xyz()),
        screen_width: screen[0],
        screen_height: screen[1],
    }
}

// The level's collision, under everything else, furthest first, faded by distance from where it's drawn round (Lara or
// where she's aiming). Without anti-aliasing, which takes several times the vertices.
fn draw_meshes(
    draw_list: &DrawListMut,
    projector: &Projector,
    ui_data: &UiData,
    settings: &UiSettings,
    occlusion: &mut OcclusionBuffer,
) {
    let collision = settings.collision.get();
    let coarse_meshes = collision && settings.coarse_meshes.get();
    let detailed_meshes = collision && settings.detailed_meshes.get();
    let mesh_opacity = settings.mesh_opacity.get();
    let fade_with_distance = settings.fade_with_distance.get();
    let fade_strength = settings.fade_strength.get();
    let radii = [settings.coarse_radius.get(), settings.detailed_radius.get()];

    let flags = unsafe { &mut (*imgui::sys::igGetBackgroundDrawList()).Flags };
    let anti_aliased = *flags;
    *flags &= !((imgui::sys::ImDrawListFlags_AntiAliasedLines
        | imgui::sys::ImDrawListFlags_AntiAliasedLinesUseTex
        | imgui::sys::ImDrawListFlags_AntiAliasedFill) as i32);
    let centre = ui_data.mesh_centre.vector();
    let triangle_count = count(ui_data.triangle_count, ui_data.triangles.len());
    let mut shown = vec![];
    for triangle in ui_data.triangles[..triangle_count].iter().rev() {
        if !((triangle.detailed && detailed_meshes) || (!triangle.detailed && coarse_meshes)) {
            continue;
        }
        let mut opacity = mesh_opacity;
        if fade_with_distance {
            let radius = radii[triangle.detailed as usize].max(1.0);
            let t = (triangle_distance(&centre, triangle) / radius).min(1.0);
            opacity *= (1.0 - t).powf(fade_strength);
        }
        if opacity < MIN_OPACITY {
            continue;
        }
        if let Some(corners) = triangle_on_screen(projector, triangle) {
            shown.push((triangle, corners, opacity));
        }
    }
    if settings.cull_hidden.get() {
        occlusion.reset(projector.screen_width, projector.screen_height);
        for (i, (_, corners, _)) in shown.iter().enumerate() {
            occlusion.add(i as u32, projector, corners);
        }
        let visible = occlusion.visible(shown.len());
        let mut i = 0;
        shown.retain(|_| {
            i += 1;
            visible[i - 1]
        });
    }
    for (triangle, corners, opacity) in &shown {
        draw_triangle(draw_list, projector, triangle, corners, *opacity);
    }
    *flags = anti_aliased;
}

fn draw_objects(ui: &Ui, draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings) {
    let (interactables, npcs) = (settings.interactables.get(), settings.npcs.get());
    for object in &ui_data.objects[..count(ui_data.object_count, ui_data.objects.len())] {
        let shown = match object.category {
            ObjectCategory::Interactable => interactables,
            ObjectCategory::Npc => npcs,
            ObjectCategory::Named => true,
        };
        if shown {
            draw_box(ui, draw_list, projector, object);
        }
    }
}

// Trigger volumes filled in faintly, outlined (white if Lara's in them) and labelled above
fn draw_triggers(ui: &Ui, draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings) {
    let trigger_opacity = settings.trigger_opacity.get();
    let by_action = settings.triggers_by_action.get();
    if trigger_opacity > 0.0 {
        let face_count = count(ui_data.trigger_face_count, ui_data.trigger_faces.len());
        for face in &ui_data.trigger_faces[..face_count] {
            let world = face.corners.map(|c| c.vector());
            if let Some(corners) = face_on_screen(projector, world) {
                let fill = with_opacity(trigger_colour(by_action, face.category, face.action), trigger_opacity);
                let polygon = projector.project_polygon(&corners);
                for i in 2..polygon.len() {
                    draw_list.add_triangle(polygon[0], polygon[i - 1], polygon[i], fill).filled(true).build();
                }
            }
        }
    }
    let line_count = count(ui_data.trigger_line_count, ui_data.trigger_lines.len());
    for line in &ui_data.trigger_lines[..line_count] {
        let colour = if line.inside { INSIDE_TRIGGER_COLOUR } else { trigger_colour(by_action, line.category, line.action) };
        draw_world_line(draw_list, projector, line.a.vector(), line.b.vector(), colour);
    }
    for label in &ui_data.triggers[..count(ui_data.trigger_count, ui_data.triggers.len())] {
        if let Some(point) = projector.project(label.position.vector()) {
            // Above the top of the volume, centred
            let size = ui.calc_text_size(label.name());
            let point = [point[0] - size[0] / 2.0, point[1] - size[1]];
            draw_list.add_text(point, trigger_colour(by_action, label.category, label.action), label.name());
        }
    }
}

// Where she's aiming (which the meshes are drawn round then), her collision, and a circle where the meshes are drawn
// round
fn draw_lara(draw_list: &DrawListMut, projector: &Projector, ui_data: &UiData, settings: &UiSettings) {
    let centre = ui_data.mesh_centre.vector();
    if ui_data.aiming {
        for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
            let arm = axis * AIM_MARKER_SIZE;
            draw_world_line(draw_list, projector, centre - arm, centre + arm, AIM_MARKER_COLOUR);
        }
    }
    if settings.collision.get() && settings.lara_collision.get() {
        let opacity = |colour| with_opacity(colour, settings.mesh_opacity.get());
        for capsule in &ui_data.capsules[..count(ui_data.capsule_count, ui_data.capsules.len())] {
            draw_capsule(draw_list, projector, capsule, opacity(LARA_COLOUR));
        }
        for probe in &ui_data.probes[..count(ui_data.probe_count, ui_data.probes.len())] {
            draw_probe(draw_list, projector, probe, opacity(PROBE_COLOUR));
        }
        let proxy_count = count(ui_data.proxy_count, ui_data.proxies.len());
        for (i, (proxy, colour)) in ui_data.proxies[..proxy_count].iter().zip(PROXY_COLOURS).enumerate() {
            let colour = if i == 0 && ui_data.proxy_touching { PUSHING_COLOUR } else { colour };
            draw_capsule(draw_list, projector, proxy, opacity(colour));
        }
        // The camera and weapon only collide while she aims
        if ui_data.attached_active {
            let attached_count = count(ui_data.attached_count, ui_data.attached.len());
            for (capsule, touching) in ui_data.attached[..attached_count].iter().zip(ui_data.attached_touching) {
                let colour = if touching { PUSHING_COLOUR } else { COLLISION_COLOUR };
                draw_capsule(draw_list, projector, capsule, opacity(colour));
            }
        }
    }
    if let Some(point) = projector.project(centre) {
        draw_list.add_circle(point, 6.0, LARA_COLOUR).thickness(2.0).build();
    }
}

hudhook::hudhook!(GameHooks, TombHelperHud::new());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_triangles_hide_ones_behind_them() {
        let mut buffer = OcclusionBuffer::default();
        buffer.reset(40.0 * OCCLUSION_SCALE, 40.0 * OCCLUSION_SCALE);
        // Depth is 1 / w, so 0.5 is nearer than 0.1. The far one is entirely behind the near one, the other only partly.
        buffer.rasterise(0, [[5., 5., 0.1], [15., 5., 0.1], [5., 15., 0.1]]);
        buffer.rasterise(1, [[0., 0., 0.5], [30., 0., 0.5], [0., 30., 0.5]]);
        buffer.rasterise(2, [[20., 20., 0.1], [39., 20., 0.1], [20., 39., 0.1]]);
        assert_eq!(buffer.visible(3), [false, true, true]);
    }
}

impl TombHelperHud {
    // F11 (`ToggleSettingsWindow`) hides it, leaving what's drawn in the world
    fn settings_window(
        &self,
        ui: &Ui,
        ui_data: &UiData,
        settings: &UiSettings,
        screen_width: f32,
        object_count: usize,
        triangle_count: usize,
    ) {
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
                    ui.text(format!("{} objects", object_count));
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
                            "{} triangles ({:.1}ms to find, {:.1}ms to draw)",
                            triangle_count, ui_data.triangle_milliseconds, self.draw_milliseconds
                        ));
                        collision_settings(ui, ui_data, settings);
                        ui.unindent();
                    }
                }
            });
    }
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
        let kinds = (0..16)
            .filter(|&bit| ui_data.mesh_kind_counts[bit] != 0)
            .map(|bit| (1 << bit, format!("{:X} ({})##kind", 1 << bit, ui_data.mesh_kind_counts[bit]), None))
            .collect();
        bit_checkboxes(ui, &settings.other_kinds, kinds, &[155.0, 280.0]);
        ui.unindent();
    }
    slider(ui, "Opacity", 0.0, 1.0, "%.2f", &settings.mesh_opacity);
    checkbox(ui, "Fade with distance from Lara", &settings.fade_with_distance);
    slider(ui, "Fade strength", 1.0, MAX_FADE_STRENGTH, "%.1f", &settings.fade_strength);
    checkbox(ui, "Hide triangles behind others", &settings.cull_hidden);
    checkbox(ui, "Round where Lara's aiming, while she aims", &settings.follow_aim);
}
