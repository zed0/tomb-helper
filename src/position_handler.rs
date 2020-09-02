use crate::action::Action;
use crate::handler::Handler;
use crate::process_details::{AddressOffsets, AddressType};
use crate::tracked_memory::TrackedMemory;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;
use std::fmt;
use std::io;

#[derive(Debug, Clone)]
pub struct PositionHandler {
    active: bool,
    position: TrackedPosition,
    saved_position: TrackedPosition,
    handle: ProcessHandle,
    name: String,
}

impl PositionHandler {
    pub fn new_position_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
    ) -> Option<PositionHandler> {
        println!("Loading position handler...");

        Some(PositionHandler {
            active: false,
            position: TrackedPosition::new(
                address_offsets.get(&AddressType::XPosition)?.clone(),
                address_offsets.get(&AddressType::YPosition)?.clone(),
                address_offsets.get(&AddressType::ZPosition)?.clone(),
                *arch,
                *base_addr,
            ),
            saved_position: TrackedPosition::new(
                address_offsets.get(&AddressType::XPosition)?.clone(),
                address_offsets.get(&AddressType::YPosition)?.clone(),
                address_offsets.get(&AddressType::ZPosition)?.clone(),
                *arch,
                *base_addr,
            ),
            handle: *handle,
            name: "position".to_string(),
        })
    }

    pub fn new_look_at_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
    ) -> Option<PositionHandler> {
        println!("Loading look at position handler...");

        Some(PositionHandler {
            active: false,
            position: TrackedPosition::new(
                address_offsets.get(&AddressType::XLookAt)?.clone(),
                address_offsets.get(&AddressType::YLookAt)?.clone(),
                address_offsets.get(&AddressType::ZLookAt)?.clone(),
                *arch,
                *base_addr,
            ),
            saved_position: TrackedPosition::new(
                address_offsets.get(&AddressType::XLookAt)?.clone(),
                address_offsets.get(&AddressType::YLookAt)?.clone(),
                address_offsets.get(&AddressType::ZLookAt)?.clone(),
                *arch,
                *base_addr,
            ),
            handle: *handle,
            name: "look at position".to_string(),
        })
    }
}

impl Handler for PositionHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if self.active {
            self.position.apply_to_game(self.handle)?;
        }
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::ToggleActive {} => {
                if self.active {
                    self.active = false;
                    println!("Deactivated {} handler", self.name);
                } else {
                    self.active = true;
                    match self.position.fetch_from_game(self.handle) {
                        Err(msg) => eprintln!("Error activating {} handler: {}", self.name, msg),
                        Ok(()) => println!("Activated {} handler", self.name),
                    }
                }
            }
            Action::StorePosition {} => {
                self.saved_position = self.position.clone();
                self.saved_position.fetch_from_game(self.handle)?;
                println!("Stored {}! {:}", self.name, self.saved_position);
            }
            Action::RestorePosition {} => {
                self.position = self.saved_position.clone();
                self.position.apply_to_game(self.handle)?;
                println!("Restored {}! {:}", self.name, self.position);
            }
            Action::Forward { distance } => {
                if self.active {
                    self.position.x.data += distance;
                }
            }
            Action::Backward { distance } => {
                if self.active {
                    self.position.x.data -= distance;
                }
            }
            Action::Left { distance } => {
                if self.active {
                    self.position.y.data += distance;
                }
            }
            Action::Right { distance } => {
                if self.active {
                    self.position.y.data -= distance;
                }
            }
            Action::Up { distance } => {
                if self.active {
                    self.position.z.data += distance;
                }
            }
            Action::Down { distance } => {
                if self.active {
                    self.position.z.data -= distance;
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct TrackedPosition {
    x: TrackedMemory<f32>,
    y: TrackedMemory<f32>,
    z: TrackedMemory<f32>,
}

impl TrackedPosition {
    fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.fetch_from_game(handle)?;
        self.y.fetch_from_game(handle)?;
        self.z.fetch_from_game(handle)?;
        Ok(())
    }

    fn apply_to_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.apply_to_game(handle)?;
        self.y.apply_to_game(handle)?;
        self.z.apply_to_game(handle)?;
        Ok(())
    }

    fn new(
        x_offsets: Vec<usize>,
        y_offsets: Vec<usize>,
        z_offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedPosition {
        TrackedPosition {
            x: TrackedMemory::new(0.0, x_offsets, arch, base_addr),
            y: TrackedMemory::new(0.0, y_offsets, arch, base_addr),
            z: TrackedMemory::new(0.0, z_offsets, arch, base_addr),
        }
    }
}

impl fmt::Display for TrackedPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x.data, self.y.data, self.z.data)
    }
}
