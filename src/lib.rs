use imgui::*;
use hudhook::*;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use crate::ui_ipc_data::SharedMemoryManager;
use crate::tracing::{error, warn};

mod ui_ipc_data;
mod position;

use nalgebra::{Matrix4, Vector4, Vector3};

/// Projects a 3D world position to 2D screen coordinates.
/// 
/// - `world_pos`: 3D position in world space
/// - `extrinsic`: 4x4 world-to-camera matrix
/// - `intrinsic`: 4x4 projection matrix
/// - `screen_width`, `screen_height`: viewport size in pixels
///
/// Returns Some((x, y)) if visible, or None if behind camera
// fn world_to_screen(
//     world_pos: Vector3<f32>,
//     extrinsic: &Matrix4<f32>,
//     intrinsic: &Matrix4<f32>,
//     screen_width: f32,
//     screen_height: f32,
// ) -> Option<(f32, f32)> {
//     // Convert to homogeneous coordinate
//     let world_pos_h = Vector4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
//
//     // World → Camera
//     let cam_space = extrinsic * world_pos_h;
//     let cam_space = Vector4::new(cam_space.x, cam_space.y, -cam_space.z, cam_space.w);
//     // let cam_space = Vector4::new(100., 0., -10., 1.0);
//     // warn!("cam_space: {}", cam_space);
//
//     // Discard points behind the camera
//     // if cam_space.z >= 0.0 {
//     //     return None;
//     // }
//
//     // Camera → Clip (projection)
//     let clip_space = intrinsic * cam_space;
//     // warn!("clip_space: {}", clip_space);
//
//     // Perspective divide to get NDC
//     if clip_space.w.abs() < 1e-6 {
//         return None;
//     }
//     let ndc = clip_space.xyz() / clip_space.w;
//
//     // NDC [-1,1] → screen coordinates
//     let x_screen = (ndc.x * 0.5 + 0.5) * screen_width;
//     let y_screen = (1.0 - (ndc.y * 0.5 + 0.5)) * screen_height; // y flipped for typical screen space
//     // let y_screen = (ndc.y * 0.5 + 0.5) * screen_height;
//
//     Some((x_screen, y_screen))
// }

pub fn world_to_screen(
    world_pos: Vector3<f32>,
    view_matrix: &Matrix4<f32>,
    projection_matrix: &Matrix4<f32>,
    screen_width: f32,
    screen_height: f32,
) -> Option<(f32, f32)> {
    // Step 1: Convert to homogeneous coordinates
    let world_pos_h = Vector4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);

    // Step 2: Transform into clip space (projection * view * world)
    let clip_space = projection_matrix * view_matrix * world_pos_h;

    // Step 3: Perspective divide (check if w != 0)
    if clip_space.w.abs() < f32::EPSILON {
        return None;
    }
    let ndc = clip_space.xyz() / clip_space.w;

    // Step 4: Convert NDC (-1..1) to screen coordinates (0..width/height)
    let screen_x = (ndc.x + 1.0) * 0.5 * screen_width;
    let screen_y = (1.0 - ndc.y) * 0.5 * screen_height; // flip y-axis for screen coords

    Some((screen_x, screen_y))
}

struct TombHelperHud {
    ui_memory_manager: SharedMemoryManager,
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
            ejecting: false,
        }
    }
}

impl ImguiRenderLoop for TombHelperHud {
    fn render(&mut self, ui: &mut Ui) {
        let ui_data = self.ui_memory_manager.get().current();
        if !self.ejecting && ui_data.should_exit {
            self.ejecting = true;
            error!("ejecting!");
            eject();
        }

        let screen_width = ui.io().display_size[0];
        let screen_height = ui.io().display_size[1];

        ui.window("Position")
            .position_pivot([1., 0.])
            .position([screen_width, 0.], Condition::Once)
            .size([400., 200.], Condition::Once)
            .build(|| {
                ui.text_wrapped(format!("{:?}", ui_data.position));
            });

        
        let draw_list = ui.get_foreground_draw_list();
        const RED: [f32; 3] = [1.0, 0.0, 0.0];
        const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
        for trigger in ui_data.triggers {
            if !trigger.is_some() {break};
            let trigger = trigger.unwrap();

            let origin = Vector3::new(trigger.origin.x, trigger.origin.y, trigger.origin.z);
            // warn!("origin: {}", origin);

            {
                let camera_extrinsics = Matrix4::from_column_slice(&ui_data.camera_extrinsics);
                warn!("camera_extrinsics: {}", camera_extrinsics);

                for (index, camera_intrinsics) in ui_data.camera_intrinsics.iter().enumerate() {
                    let camera_intrinsics = Matrix4::from_column_slice(camera_intrinsics);
                    // warn!("camera_intrinsics: {}", camera_intrinsics);

                    let coord_2d = world_to_screen(
                        origin,
                        &camera_extrinsics,
                        &camera_intrinsics,
                        screen_width,
                        screen_height,
                    );
                    // warn!("coord_2d: {:?}", coord_2d);

                    if coord_2d.is_some() {
                        draw_list.add_circle(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            10.0,
                            RED,
                        ).build();
                        draw_list.add_text(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            RED,
                            format!("{}", index),
                        );
                    }
                }
                {
                    let camera_intrinsics = Matrix4::identity();
                    // warn!("camera_intrinsics: {}", camera_intrinsics);

                    let coord_2d = world_to_screen(
                        origin,
                        &camera_extrinsics,
                        &camera_intrinsics,
                        screen_width,
                        screen_height,
                    );
                    // warn!("coord_2d: {:?}", coord_2d);

                    if coord_2d.is_some() {
                        draw_list.add_circle(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            10.0,
                            RED,
                        ).build();
                        draw_list.add_text(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            RED,
                            format!("X"),
                        );
                    }
                }
            }

            {
                let camera_extrinsics = Matrix4::from_row_slice(&ui_data.camera_extrinsics);
                warn!("camera_extrinsics: {}", camera_extrinsics);

                for (index, camera_intrinsics) in ui_data.camera_intrinsics.iter().enumerate() {
                    let camera_intrinsics = Matrix4::from_row_slice(camera_intrinsics);
                    // warn!("camera_intrinsics: {}", camera_intrinsics);

                    let coord_2d = world_to_screen(
                        origin,
                        &camera_extrinsics,
                        &camera_intrinsics,
                        screen_width,
                        screen_height,
                    );
                    // warn!("coord_2d: {:?}", coord_2d);

                    if coord_2d.is_some() {
                        draw_list.add_circle(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            10.0,
                            GREEN,
                        ).build();
                        draw_list.add_text(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            GREEN,
                            format!("{}", index),
                        );
                    }
                }
                {
                    let camera_intrinsics = Matrix4::identity();
                    // warn!("camera_intrinsics: {}", camera_intrinsics);

                    let coord_2d = world_to_screen(
                        origin,
                        &camera_extrinsics,
                        &camera_intrinsics,
                        screen_width,
                        screen_height,
                    );
                    // warn!("coord_2d: {:?}", coord_2d);

                    if coord_2d.is_some() {
                        draw_list.add_circle(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            10.0,
                            GREEN,
                        ).build();
                        draw_list.add_text(
                            [coord_2d.unwrap().0, coord_2d.unwrap().1],
                            GREEN,
                            format!("X"),
                        );
                    }
                }
            }
        }
    }
}

hudhook::hudhook!(ImguiDx12Hooks, TombHelperHud::new());
