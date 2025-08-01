use crate::position::Position;
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use crate::action::Action;
use crate::handler::Handler;
use crate::process_details::{AddressOffsets, AddressType};
use std::convert::TryInto;
use std::{env, fmt, io};
use std::error::Error;
use hudhook::inject::Process;
use process_memory::{Architecture, Pid, ProcessHandle};
use crate::ui_ipc_data::{SharedMemoryManager, Trigger};

pub struct OverlayHandler {
    active: bool,
    name: String,
    handle: ProcessHandle,
    pid: Pid,
    position: TrackedPosition,
    camera_extrinsics: TrackedCameraMatrix,
    camera_intrinsics: [TrackedCameraMatrix;100],
    ui_memory_manager: SharedMemoryManager,
}

impl OverlayHandler {
    pub fn new_overlay_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
    ) -> Option<OverlayHandler> {
        println!("Loading overlay handler...");

        let mut ui_memory_manager = SharedMemoryManager::new(true);
        let manager = ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.debug = true;
        val.should_exit = false;
        manager.set_next(val);

        let mut extrinsics_offsets = address_offsets.get(&AddressType::CameraSin)?.clone();
        // let extrinsics_offsets = vec![0x2D3B1D0];
        // *extrinsics_offsets.last_mut().unwrap() += 0 * 4;
        // *extrinsics_offsets.last_mut().unwrap() += 104 * 4;
        // *extrinsics_offsets.last_mut().unwrap() += 208 * 4;

        // let intrinsics_offsets = address_offsets.get(&AddressType::CameraSin)?.clone();
        // *intrinsics_offsets.last_mut().unwrap() += 44 * 4;

        println!("extrinsics offsets: {:?}", extrinsics_offsets);
        // println!("intrinsics offsets: {:?}", intrinsics_offsets);

        Some(OverlayHandler {
            active: false,
            name: "overlay".to_string(),
            handle: *handle,
            pid: *pid,
            position: TrackedPosition::new(
                address_offsets.get(&AddressType::XPosition)?.clone(),
                address_offsets.get(&AddressType::YPosition)?.clone(),
                address_offsets.get(&AddressType::ZPosition)?.clone(),
                *arch,
                *base_addr,
            ),
            camera_extrinsics: TrackedCameraMatrix::new(
                extrinsics_offsets.clone(),
                *arch,
                *base_addr,
            ),
            // camera_intrinsics: TrackedCameraMatrix::new(
            //     intrinsics_offsets,
            //     *arch,
            //     *base_addr,
            // ),
            camera_intrinsics: (0usize..100usize).map(|n|{
                let mut offsets = extrinsics_offsets.clone();
                *offsets.last_mut().unwrap() += n*4;
                TrackedCameraMatrix::new(offsets, *arch, *base_addr)
            }).collect::<Vec<TrackedCameraMatrix>>()
            .try_into()
            .unwrap(),
            ui_memory_manager,
        })
    }

}

impl Handler for OverlayHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.active {
            return Ok(());
        }

        self.position.fetch_from_game(self.handle)?;
        self.camera_extrinsics.fetch_from_game(self.handle)?;
        // self.camera_intrinsics.fetch_from_game(self.handle)?;
        for camera_intrinsic in &mut self.camera_intrinsics {
            camera_intrinsic.fetch_from_game(self.handle)?;
        }
        let manager = self.ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.position = self.position.to_position();
        val.camera_extrinsics = self.camera_extrinsics.to_array();
        // val.camera_intrinsics = self.camera_intrinsics.to_array();
        val.camera_intrinsics = self.camera_intrinsics.clone().map(|i| i.to_array());
        val.triggers = [
            Some(Trigger {
                origin: Position {
                    x: 44744.87,
                    y: -10983.537,
                    z: 16185.443,
                },
                size: 1.0,
            }),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        manager.set_next(val);
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::ToggleUI {} => {
                self.active = !self.active;

                if self.active {
                    let manager = self.ui_memory_manager.get_mut();
                    let mut val = manager.current();
                    val.should_exit = false;
                    manager.set_next(val);

                    println!("Injecting overlay dll");
                    let dll_path = env::current_exe().unwrap().with_file_name("tomb_helper_ui.dll");
                    Process::by_pid(&self.pid)
                        .unwrap()
                        .inject(dll_path)
                        .unwrap();
                } else {
                    println!("Ejecting injected overlay dll");
                    let manager = self.ui_memory_manager.get_mut();
                    let mut val = manager.current();
                    val.should_exit = true;
                    manager.set_next(val);
                    println!("done");
                }
            },
            _ => (),
        }
        Ok(())
    }
}

impl Drop for OverlayHandler {
    fn drop(&mut self) {
        println!("Ejecting injected overlay dll");
        let manager = self.ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.should_exit = true;
        manager.set_next(val);
        println!("done");
    }
}

#[derive(Debug, Clone)]
pub struct TrackedCameraMatrix {
    pub matrix: [TrackedMemory<f32>;16],
}

impl TrackedCameraMatrix {
    pub fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        for element in &mut self.matrix {
            element.fetch_from_game(handle)?;
        }
        Ok(())
    }

    pub fn apply_to_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        for element in &mut self.matrix {
            element.apply_to_game(handle)?;
        }
        Ok(())
    }

    pub fn new(
        matrix_offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedCameraMatrix {
        TrackedCameraMatrix {
            matrix: (0usize..16usize).map(|n|{
                let mut offsets = matrix_offsets.clone();
                *offsets.last_mut().unwrap() += n*4;
                TrackedMemory::new(0.0, offsets, arch, base_addr)
            }).collect::<Vec<TrackedMemory<f32>>>()
            .try_into()
            .unwrap()
        }
    }

    pub fn to_array(&self) -> [f32; 16] {
        self.matrix.clone().map(|e| e.data)
    }
}

impl fmt::Display for TrackedCameraMatrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}, {}, {}, {})\n({}, {}, {}, {})\n({}, {}, {}, {})\n({}, {}, {}, {})\n",
            self.matrix[0].data,
            self.matrix[1].data,
            self.matrix[2].data,
            self.matrix[3].data,
            self.matrix[4].data,
            self.matrix[5].data,
            self.matrix[6].data,
            self.matrix[7].data,
            self.matrix[8].data,
            self.matrix[9].data,
            self.matrix[10].data,
            self.matrix[11].data,
            self.matrix[12].data,
            self.matrix[13].data,
            self.matrix[14].data,
            self.matrix[15].data,
        )
    }
}
