use imgui::*;
use hudhook::*;
// TR2013 is 32-bit and DirectX 11, the others 64-bit and DirectX 12
#[cfg(target_arch = "x86")]
use hudhook::hooks::dx11::ImguiDx11Hooks as GameHooks;
#[cfg(not(target_arch = "x86"))]
use hudhook::hooks::dx12::ImguiDx12Hooks as GameHooks;
use crate::ui_ipc_data::{projection_from_lens, BoolSetting, BoxKind, Capsule, F32Setting, ObjectBox, ObjectCategory, Probe, SharedMemoryManager, SurfaceKind, Triangle, TriggerCategory, UiData, UiSettings, REQUEST_RESTORE_POSITION, REQUEST_STORE_POSITION, REQUEST_TOGGLE_FLYING, TRIGGER_ACTIONS};
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
        let ui_data = &*self.ui_data;
        if !self.ejecting && ui_data.should_exit {
            self.ejecting = true;
            error!("ejecting!");
            eject();
        }
        // The exe updates every tick while the overlay's on, even while the game's paused
        let generation = self.ui_memory_manager.get().generation();
        if generation != self.last_generation {
            self.last_generation = generation;
            self.last_update = std::time::Instant::now();
        } else if !self.ejecting && self.last_update.elapsed() > EXE_GONE {
            self.ejecting = true;
            error!("tomb-helper stopped updating, ejecting!");
            eject();
        }

        let screen_width = ui.io().display_size[0];
        let screen_height = ui.io().display_size[1];
        let object_count = count(ui_data.object_count, ui_data.objects.len());
        let triangle_count = count(ui_data.triangle_count, ui_data.triangles.len());
        let settings = &self.ui_memory_manager.get().settings;

        if settings.show_window.get() {
            self.settings_window(ui, ui_data, settings, screen_width, object_count, triangle_count);
        }
        // Read again so changes show straight away, rather than once the exe has caught up
        let collision = settings.collision.get();
        let lara_collision = collision && settings.lara_collision.get();
        let interactables = settings.interactables.get();
        let npcs = settings.npcs.get();
        let triggers = settings.triggers.get();
        let coarse_meshes = collision && settings.coarse_meshes.get();
        let detailed_meshes = collision && settings.detailed_meshes.get();
        let mesh_opacity = settings.mesh_opacity.get();
        let fade_with_distance = settings.fade_with_distance.get();
        let fade_strength = settings.fade_strength.get();
        let cull_hidden = settings.cull_hidden.get();
        let radii = [settings.coarse_radius.get(), settings.detailed_radius.get()];

        if ui_data.camera_view_address == 0 {
            self.draw_milliseconds = started.elapsed().as_secs_f32() * 1000.0;
            return;
        }
        let view = game_matrix(&read_game(ui_data.camera_view_address as usize).unwrap_or(ui_data.camera_view));
        let projection = read_game(ui_data.camera_lens_address as usize)
            .map_or(ui_data.camera_projection, |lens| projection_from_lens(&lens, ui_data.lens_fields));
        let projector = Projector {
            view_projection: game_matrix(&projection) * view,
            camera_position: view.try_inverse().map_or(Vector3::zeros(), |m| m.column(3).xyz()),
            screen_width,
            screen_height,
        };

        let draw_list = ui.get_background_draw_list();
        // Under everything else, furthest first. Without anti-aliasing, which takes several times the vertices.
        let flags = unsafe { &mut (*imgui::sys::igGetBackgroundDrawList()).Flags };
        let anti_aliased = *flags;
        *flags &= !((imgui::sys::ImDrawListFlags_AntiAliasedLines
            | imgui::sys::ImDrawListFlags_AntiAliasedLinesUseTex
            | imgui::sys::ImDrawListFlags_AntiAliasedFill) as i32);
        // Faded by distance from where they're drawn round, Lara or where she's aiming
        let centre = ui_data.mesh_centre;
        let lara = centre.vector();
        let mut shown = vec![];
        for triangle in ui_data.triangles[..triangle_count].iter().rev() {
            if !((triangle.detailed && detailed_meshes) || (!triangle.detailed && coarse_meshes)) {
                continue;
            }
            let mut opacity = mesh_opacity;
            if fade_with_distance {
                let radius = radii[triangle.detailed as usize].max(1.0);
                let t = (triangle_distance(&lara, triangle) / radius).min(1.0);
                opacity *= (1.0 - t).powf(fade_strength);
            }
            if opacity < MIN_OPACITY {
                continue;
            }
            if let Some(corners) = triangle_on_screen(&projector, triangle) {
                shown.push((triangle, corners, opacity));
            }
        }
        if cull_hidden {
            self.occlusion.reset(screen_width, screen_height);
            for (i, (_, corners, _)) in shown.iter().enumerate() {
                self.occlusion.add(i as u32, &projector, corners);
            }
            let visible = self.occlusion.visible(shown.len());
            let mut i = 0;
            shown.retain(|_| {
                i += 1;
                visible[i - 1]
            });
        }
        for (triangle, corners, opacity) in &shown {
            draw_triangle(&draw_list, &projector, triangle, corners, *opacity);
        }
        *flags = anti_aliased;
        for object in &ui_data.objects[..object_count] {
            let shown = match object.category {
                ObjectCategory::Interactable => interactables,
                ObjectCategory::Npc => npcs,
                ObjectCategory::Named => true,
            };
            if shown {
                draw_box(ui, &draw_list, &projector, object);
            }
        }
        if triggers {
            let trigger_opacity = settings.trigger_opacity.get();
            let by_action = settings.triggers_by_action.get();
            if trigger_opacity > 0.0 {
                let face_count = count(ui_data.trigger_face_count, ui_data.trigger_faces.len());
                for face in &ui_data.trigger_faces[..face_count] {
                    let world = face.corners.map(|c| c.vector());
                    if let Some(corners) = face_on_screen(&projector, world) {
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
                let colour =
                    if line.inside { INSIDE_TRIGGER_COLOUR } else { trigger_colour(by_action, line.category, line.action) };
                draw_world_line(&draw_list, &projector, line.a.vector(), line.b.vector(), colour);
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
        // Where she's aiming, which the meshes are drawn round
        if ui_data.aiming {
            let centre = ui_data.mesh_centre;
            let centre = centre.vector();
            for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
                let arm = axis * AIM_MARKER_SIZE;
                draw_world_line(&draw_list, &projector, centre - arm, centre + arm, AIM_MARKER_COLOUR);
            }
        }
        if lara_collision {
            let opacity = |colour| with_opacity(colour, mesh_opacity);
            for capsule in &ui_data.capsules[..count(ui_data.capsule_count, ui_data.capsules.len())] {
                draw_capsule(&draw_list, &projector, capsule, opacity(LARA_COLOUR));
            }
            for probe in &ui_data.probes[..count(ui_data.probe_count, ui_data.probes.len())] {
                draw_probe(&draw_list, &projector, probe, opacity(PROBE_COLOUR));
            }
            let proxy_count = count(ui_data.proxy_count, ui_data.proxies.len());
            for (i, (proxy, colour)) in ui_data.proxies[..proxy_count].iter().zip(PROXY_COLOURS).enumerate() {
                let colour = if i == 0 && ui_data.proxy_touching { PUSHING_COLOUR } else { colour };
                draw_capsule(&draw_list, &projector, proxy, opacity(colour));
            }
            // The camera and weapon only collide while she aims
            if ui_data.attached_active {
                let attached_count = count(ui_data.attached_count, ui_data.attached.len());
                for (capsule, touching) in ui_data.attached[..attached_count].iter().zip(ui_data.attached_touching) {
                    let colour = if touching { PUSHING_COLOUR } else { COLLISION_COLOUR };
                    draw_capsule(&draw_list, &projector, capsule, opacity(colour));
                }
            }
        }

        if let Some(point) = projector.project(lara) {
            draw_list.add_circle(point, 6.0, LARA_COLOUR).thickness(2.0).build();
        }
        self.draw_milliseconds = started.elapsed().as_secs_f32() * 1000.0;
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
        let request = |bit: u32| {
            settings.requests.fetch_or(bit, std::sync::atomic::Ordering::SeqCst);
        };
        ui.window("tomb-helper")
            .position_pivot([1., 0.])
            .position([screen_width, 0.], Condition::Once)
            .size([400., 650.], Condition::Once)
            .build(|| {
                if ui.collapsing_header("Lara", TreeNodeFlags::DEFAULT_OPEN) {
                    let position = ui_data.position;
                    for (axis, value) in [("X", position.x), ("Y", position.y), ("Z", position.z)] {
                        ui.text(format!("{} {:>12.2}", axis, value));
                    }
                }
                if ui_data.position_tools && ui.collapsing_header("Position tools", TreeNodeFlags::DEFAULT_OPEN) {
                    let mut flying = ui_data.flying;
                    if ui.checkbox("Flight mode", &mut flying) {
                        request(REQUEST_TOGGLE_FLYING);
                    }
                    if ui.is_item_hovered() {
                        ui.tooltip_text("Holds Lara where she is; the movement hotkeys (W/A/S/D, Space and C by default) move her");
                    }
                    if ui.button("Save position") {
                        request(REQUEST_STORE_POSITION);
                    }
                    if ui_data.has_saved_position {
                        ui.same_line();
                        if ui.button("Restore position") {
                            request(REQUEST_RESTORE_POSITION);
                        }
                        let saved = ui_data.saved_position;
                        ui.text("Saved:");
                        for (axis, value) in [("X", saved.x), ("Y", saved.y), ("Z", saved.z)] {
                            ui.text(format!("{} {:>12.2}", axis, value));
                        }
                    } else {
                        ui.text_disabled("Nothing saved yet");
                    }
                }
                if ui.collapsing_header("Overlay", TreeNodeFlags::DEFAULT_OPEN) {
                    ui.text(format!("{} objects", object_count));
                    for (label, setting) in [
                        ("Interactable objects", &settings.interactables),
                        ("Characters and animals", &settings.npcs),
                        ("Trigger volumes", &settings.triggers),
                    ] {
                        checkbox(ui, label, setting);
                    }
                    if settings.triggers.get() {
                        ui.indent();
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
                            let mut actions = settings.trigger_actions.get();
                            if ui.small_button("All") {
                                settings.trigger_actions.set(u32::MAX);
                            }
                            ui.same_line();
                            if ui.small_button("None") {
                                settings.trigger_actions.set(0);
                            }
                            // Two columns, they're short
                            for (i, action) in TRIGGER_ACTIONS.iter().enumerate() {
                                if i % 2 == 1 {
                                    ui.same_line_with_pos(190.);
                                }
                                let mut value = actions & 1 << i != 0;
                                let style = ui.push_style_color(StyleColor::Text, TRIGGER_ACTION_COLOURS[i]);
                                if ui.checkbox(action, &mut value) {
                                    actions ^= 1 << i;
                                    settings.trigger_actions.set(actions);
                                }
                                style.pop();
                            }
                        } else {
                            let mut categories = settings.trigger_categories.get();
                            for category in TriggerCategory::ALL {
                                let (colour, label) = trigger_style(category);
                                let mut value = categories & category.bit() != 0;
                                let style = ui.push_style_color(StyleColor::Text, colour);
                                if ui.checkbox(label, &mut value) {
                                    categories ^= category.bit();
                                    settings.trigger_categories.set(categories);
                                }
                                style.pop();
                            }
                        }
                        slider(ui, "Opacity##triggers", 0.0, 1.0, "%.2f", &settings.trigger_opacity);
                        ui.unindent();
                    }
                    checkbox(ui, "Collision meshes (F10)", &settings.collision);
                    if settings.collision.get() {
                        ui.indent();
                        ui.text(format!(
                            "{} triangles ({:.1}ms to find, {:.1}ms to draw)",
                            triangle_count, ui_data.triangle_milliseconds, self.draw_milliseconds
                        ));
                        checkbox(ui, "Lara's collision", &settings.lara_collision);
                        checkbox(ui, "Movement collision (Lara's, 0x2)", &settings.coarse_meshes);
                        slider(ui, "Radius##coarse", 0.0, MAX_MESH_RADIUS, "%.0f", &settings.coarse_radius);
                        checkbox(ui, "Other collision", &settings.detailed_meshes);
                        slider(ui, "Radius##detailed", 0.0, MAX_MESH_RADIUS, "%.0f", &settings.detailed_radius);
                        if settings.detailed_meshes.get() {
                            // Which kinds of collision (bits of the meshes' filters), and how many meshes in the level have
                            // each; what they're for isn't known, besides 0x2 (Lara's movement)
                            ui.indent();
                            let mut kinds = settings.other_kinds.get();
                            let mut shown = 0;
                            for bit in 0..16 {
                                let count = ui_data.mesh_kind_counts[bit];
                                if count == 0 {
                                    continue;
                                }
                                if shown % 3 != 0 {
                                    ui.same_line_with_pos(shown as f32 % 3.0 * 125.0 + 30.0);
                                }
                                shown += 1;
                                let mut value = kinds & 1 << bit != 0;
                                if ui.checkbox(format!("{:X} ({})##kind", 1 << bit, count), &mut value) {
                                    kinds ^= 1 << bit;
                                    settings.other_kinds.set(kinds);
                                }
                            }
                            ui.unindent();
                        }
                        slider(ui, "Opacity", 0.0, 1.0, "%.2f", &settings.mesh_opacity);
                        checkbox(ui, "Fade with distance from Lara", &settings.fade_with_distance);
                        slider(ui, "Fade strength", 1.0, MAX_FADE_STRENGTH, "%.1f", &settings.fade_strength);
                        checkbox(ui, "Hide triangles behind others", &settings.cull_hidden);
                        checkbox(ui, "Round where Lara's aiming, while she aims", &settings.follow_aim);
                        ui.unindent();
                    }
                }
            });
    }
}
