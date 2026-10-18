use imgui::*;
use hudhook::*;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use crate::ui_ipc_data::{projection_from_lens, BoxKind, Capsule, ObjectBox, Probe, SharedMemoryManager, SurfaceKind, Triangle, UiData};
use crate::tracing::{error, warn};

mod ui_ipc_data;
mod position;

use nalgebra::{Matrix4, Vector3, Vector4};
use std::mem::{size_of, MaybeUninit};
use winapi::um::memoryapi::ReadProcessMemory;
use winapi::um::processthreadsapi::GetCurrentProcess;

// Points closer to the camera plane than this (in clip space w) are clipped off
const NEAR_CLIP_W: f32 = 1.0;
// Lines per circle when drawing capsules
const CAPSULE_SEGMENTS: usize = 16;

const MODEL_COLOUR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const ESTIMATED_COLOUR: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
const LARA_COLOUR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
const PROBE_COLOUR: [f32; 4] = [0.0, 1.0, 1.0, 1.0];
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
const SURFACE_FILL_ALPHA: f32 = 0.12;

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
        let mut screen = vec![];
        for (i, &a) in corners.iter().enumerate() {
            let b = corners[(i + 1) % corners.len()];
            if a.w >= NEAR_CLIP_W {
                screen.push(self.to_screen(a));
            }
            if (a.w >= NEAR_CLIP_W) != (b.w >= NEAR_CLIP_W) {
                let t = (a.w - NEAR_CLIP_W) / (a.w - b.w);
                screen.push(self.to_screen(a + (b - a) * t));
            }
        }
        screen
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

fn draw_box(draw_list: &DrawListMut, projector: &Projector, object: &ObjectBox) {
    let colour = match object.kind {
        BoxKind::Model => MODEL_COLOUR,
        BoxKind::Estimated => ESTIMATED_COLOUR,
        BoxKind::Lara => LARA_COLOUR,
    };
    let model = game_matrix(&object.transform);
    let (min, max) = (object.min, object.max);
    // Corner i uses max for each axis whose bit is set in i
    let corners: Vec<Vector4<f32>> = (0..8)
        .map(|i| {
            let point = Vector3::new(
                if i & 1 != 0 { max.x } else { min.x },
                if i & 2 != 0 { max.y } else { min.y },
                if i & 4 != 0 { max.z } else { min.z },
            );
            projector.to_clip(&model, point)
        })
        .collect();

    for a in 0..8 {
        for axis in [1, 2, 4] {
            if a & axis == 0 {
                if let Some((from, to)) = projector.project_line(corners[a], corners[a | axis]) {
                    draw_list.add_line(from, to, colour).thickness(1.5).build();
                }
            }
        }
    }

    let top = (corners[4] + corners[5] + corners[6] + corners[7]) / 4.0;
    if top.w >= NEAR_CLIP_W {
        draw_list.add_text(projector.to_screen(top), colour, object.name());
    }
}

fn draw_world_line(draw_list: &DrawListMut, projector: &Projector, a: Vector3<f32>, b: Vector3<f32>, colour: [f32; 4]) {
    if let Some((from, to)) = projector.project_line(projector.world_to_clip(a), projector.world_to_clip(b)) {
        draw_list.add_line(from, to, colour).thickness(1.5).build();
    }
}

fn draw_capsule(draw_list: &DrawListMut, projector: &Projector, capsule: &Capsule, colour: [f32; 4]) {
    let a = Vector3::new(capsule.a.x, capsule.a.y, capsule.a.z);
    let b = Vector3::new(capsule.b.x, capsule.b.y, capsule.b.z);
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

fn draw_probe(draw_list: &DrawListMut, projector: &Projector, probe: &Probe) {
    let vector = |p: &crate::position::Position| Vector3::new(p.x, p.y, p.z);
    draw_world_line(draw_list, projector, vector(&probe.start), vector(&probe.end), PROBE_COLOUR);
    if probe.has_hit {
        let hit = vector(&probe.hit);
        for axis in [Vector3::x(), Vector3::y(), Vector3::z()] {
            let offset = axis * PROBE_HIT_SIZE;
            draw_world_line(draw_list, projector, hit - offset, hit + offset, PROBE_COLOUR);
        }
    }
}

fn draw_triangle(draw_list: &DrawListMut, projector: &Projector, triangle: &Triangle) {
    let colour = match triangle.kind {
        SurfaceKind::Floor => FLOOR_COLOUR,
        SurfaceKind::Wall => WALL_COLOUR,
        SurfaceKind::Ceiling => CEILING_COLOUR,
    };
    let world = triangle.corners.map(|c| Vector3::new(c.x, c.y, c.z));
    // Seen from behind. Triangles are wound anticlockwise seen from the side they face.
    let normal = (world[1] - world[0]).cross(&(world[2] - world[0]));
    if normal.dot(&(projector.camera_position - world[0])) <= 0.0 {
        return;
    }
    let corners = world.map(|c| projector.world_to_clip(c));
    // All off the same side of the screen, or behind the camera
    let outside = |test: fn(&Vector4<f32>) -> bool| corners.iter().all(test);
    if outside(|c| c.x < -c.w)
        || outside(|c| c.x > c.w)
        || outside(|c| c.y < -c.w)
        || outside(|c| c.y > c.w)
        || outside(|c| c.w < NEAR_CLIP_W)
    {
        return;
    }

    let fill = [colour[0], colour[1], colour[2], SURFACE_FILL_ALPHA];
    let polygon = projector.project_polygon(&corners);
    for i in 2..polygon.len() {
        draw_list.add_triangle(polygon[0], polygon[i - 1], polygon[i], fill).filled(true).build();
    }
    for i in 0..3 {
        if let Some((from, to)) = projector.project_line(corners[i], corners[(i + 1) % 3]) {
            draw_list.add_line(from, to, colour).build();
        }
    }
}

struct TombHelperHud {
    ui_memory_manager: SharedMemoryManager,
    // Last complete copy, used when the exe was mid write
    ui_data: Box<UiData>,
    // Where the next copy is read to
    scratch: Box<UiData>,
    ejecting: bool,
    // How long the last frame's drawing took
    draw_milliseconds: f32,
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
        let ui_data = &*self.ui_data;
        if !self.ejecting && ui_data.should_exit {
            self.ejecting = true;
            error!("ejecting!");
            eject();
        }

        let screen_width = ui.io().display_size[0];
        let screen_height = ui.io().display_size[1];
        let object_count = ui_data.object_count.min(ui_data.objects.len());
        let triangle_count = ui_data.triangle_count.min(ui_data.triangles.len());

        ui.window("Position")
            .position_pivot([1., 0.])
            .position([screen_width, 0.], Condition::Once)
            .size([400., 200.], Condition::Once)
            .build(|| {
                ui.text_wrapped(format!("{:?}", ui_data.position));
                ui.text(format!("{} objects", object_count));
                ui.text(format!(
                    "{} collision triangles ({:.1}ms to find, {:.1}ms to draw)",
                    triangle_count, ui_data.triangle_milliseconds, self.draw_milliseconds
                ));
            });

        let view = game_matrix(&read_game(ui_data.camera_view_address).unwrap_or(ui_data.camera_view));
        let projection = read_game(ui_data.camera_lens_address)
            .map_or(ui_data.camera_projection, |lens| projection_from_lens(&lens));
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
        for triangle in ui_data.triangles[..triangle_count].iter().rev() {
            draw_triangle(&draw_list, &projector, triangle);
        }
        *flags = anti_aliased;
        for object in &ui_data.objects[..object_count] {
            draw_box(&draw_list, &projector, object);
        }
        for capsule in &ui_data.capsules[..ui_data.capsule_count.min(ui_data.capsules.len())] {
            draw_capsule(&draw_list, &projector, capsule, LARA_COLOUR);
        }
        for probe in &ui_data.probes[..ui_data.probe_count.min(ui_data.probes.len())] {
            draw_probe(&draw_list, &projector, probe);
        }
        let proxy_count = ui_data.proxy_count.min(ui_data.proxies.len());
        for (i, (proxy, colour)) in ui_data.proxies[..proxy_count].iter().zip(PROXY_COLOURS).enumerate() {
            let colour = if i == 0 && ui_data.proxy_touching { PUSHING_COLOUR } else { colour };
            draw_capsule(&draw_list, &projector, proxy, colour);
        }
        // The camera and weapon only collide while she aims
        if ui_data.attached_active {
            let attached_count = ui_data.attached_count.min(ui_data.attached.len());
            for (capsule, touching) in ui_data.attached[..attached_count].iter().zip(ui_data.attached_touching) {
                draw_capsule(&draw_list, &projector, capsule, if touching { PUSHING_COLOUR } else { COLLISION_COLOUR });
            }
        }

        let lara = Vector3::new(ui_data.position.x, ui_data.position.y, ui_data.position.z);
        if let Some(point) = projector.project(lara) {
            draw_list.add_circle(point, 6.0, LARA_COLOUR).thickness(2.0).build();
        }
        self.draw_milliseconds = started.elapsed().as_secs_f32() * 1000.0;
    }
}

hudhook::hudhook!(ImguiDx12Hooks, TombHelperHud::new());
