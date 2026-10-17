use imgui::*;
use hudhook::*;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use crate::ui_ipc_data::{ObjectBox, SharedMemoryManager, UiData};
use crate::tracing::{error, warn};

mod ui_ipc_data;
mod position;

use nalgebra::{Matrix4, Vector3, Vector4};

// Points closer to the camera plane than this (in clip space w) are clipped off
const NEAR_CLIP_W: f32 = 1.0;

const BOX_COLOUR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const LARA_COLOUR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];

// The game stores matrices row-major for row vectors (`v * M`). Reading the same floats column-major
// gives the transposed matrix, which is the equivalent for nalgebra's column vectors (`M * v`).
fn game_matrix(elements: &[f32; 16]) -> Matrix4<f32> {
    Matrix4::from_column_slice(elements)
}

struct Projector {
    view_projection: Matrix4<f32>,
    screen_width: f32,
    screen_height: f32,
}

impl Projector {
    fn to_clip(&self, model: &Matrix4<f32>, point: Vector3<f32>) -> Vector4<f32> {
        self.view_projection * model * point.push(1.0)
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
        let clip = self.to_clip(&Matrix4::identity(), point);
        if clip.w < NEAR_CLIP_W {
            return None;
        }
        Some(self.to_screen(clip))
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
                    draw_list.add_line(from, to, BOX_COLOUR).thickness(1.5).build();
                }
            }
        }
    }

    let top = (corners[4] + corners[5] + corners[6] + corners[7]) / 4.0;
    if top.w >= NEAR_CLIP_W {
        draw_list.add_text(projector.to_screen(top), BOX_COLOUR, object.name());
    }
}

struct TombHelperHud {
    ui_memory_manager: SharedMemoryManager,
    // Last complete copy, used when the exe was mid write
    ui_data: UiData,
    ejecting: bool,
}

impl TombHelperHud {
    fn new() -> Self {
        let ui_memory_manager = SharedMemoryManager::new(false);
        let ui_data = ui_memory_manager.get().current();

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
            ejecting: false,
        }
    }
}

impl ImguiRenderLoop for TombHelperHud {
    fn render(&mut self, ui: &mut Ui) {
        if let Some(ui_data) = self.ui_memory_manager.get().try_current() {
            self.ui_data = ui_data;
        }
        let ui_data = self.ui_data;
        if !self.ejecting && ui_data.should_exit {
            self.ejecting = true;
            error!("ejecting!");
            eject();
        }

        let screen_width = ui.io().display_size[0];
        let screen_height = ui.io().display_size[1];
        let object_count = ui_data.object_count.min(ui_data.objects.len());

        ui.window("Position")
            .position_pivot([1., 0.])
            .position([screen_width, 0.], Condition::Once)
            .size([400., 200.], Condition::Once)
            .build(|| {
                ui.text_wrapped(format!("{:?}", ui_data.position));
                ui.text(format!("{} objects", object_count));
            });

        let projector = Projector {
            view_projection: game_matrix(&ui_data.camera_projection) * game_matrix(&ui_data.camera_view),
            screen_width,
            screen_height,
        };

        let draw_list = ui.get_background_draw_list();
        for object in &ui_data.objects[..object_count] {
            draw_box(&draw_list, &projector, object);
        }

        let lara = Vector3::new(ui_data.position.x, ui_data.position.y, ui_data.position.z);
        if let Some(point) = projector.project(lara) {
            draw_list.add_circle(point, 6.0, LARA_COLOUR).thickness(2.0).build();
        }
    }
}

hudhook::hudhook!(ImguiDx12Hooks, TombHelperHud::new());
