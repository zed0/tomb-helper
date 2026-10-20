//! Saving and restoring Lara's position, and flight mode ("active"), which holds her where she is by writing her
//! position every tick, so the movement actions can move her (relative to the camera, by `CameraSin`/`CameraCos`).
//! Movement does nothing while it's off.

use crate::action::Action;
use crate::handler::{Handler, HelperState};
use crate::process_details::{AddressOffsets, AddressType};
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;

#[derive(Debug, Clone)]
pub struct PositionHandler {
    active: bool,
    position: TrackedPosition,
    saved_position: TrackedPosition,
    // Something's been stored in `saved_position`
    saved: bool,
    // Lara's (rather than the look at position), which the overlay shows
    is_lara: bool,
    camera_rotation: Option<(TrackedMemory<f32>, TrackedMemory<f32>)>,
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
            camera_rotation: match (
                address_offsets.get(&AddressType::CameraSin),
                address_offsets.get(&AddressType::CameraCos),
            ) {
                (Some(sin), Some(cos)) => Some((
                    TrackedMemory::new(0.0, sin.clone(), *arch, *base_addr),
                    TrackedMemory::new(0.0, cos.clone(), *arch, *base_addr),
                )),
                _ => None,
            },
            handle: *handle,
            name: "position".to_string(),
            saved: false,
            is_lara: true,
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
            camera_rotation: None,
            handle: *handle,
            name: "look at position".to_string(),
            saved: false,
            is_lara: false,
        })
    }
}

impl Handler for PositionHandler {
    fn report(&self, state: &mut HelperState) {
        if self.is_lara {
            state.position_tools = true;
            state.saved_position = if self.saved { Some(self.saved_position.to_position()) } else { None };
            state.flying = self.active;
        }
    }

    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if self.active {
            self.position.apply_to_game(self.handle)?;
            match &mut self.camera_rotation {
                Some((sin, cos)) => {
                    sin.fetch_from_game(self.handle)?;
                    cos.fetch_from_game(self.handle)?;
                }
                _ => {}
            }
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
                self.saved = true;
                println!("Stored {}! {:}", self.name, self.saved_position);
            }
            Action::RestorePosition {} => {
                self.position = self.saved_position.clone();
                self.position.apply_to_game(self.handle)?;
                println!("Restored {}! {:}", self.name, self.position);
            }
            Action::Forward { distance } => {
                if self.active {
                    match &self.camera_rotation {
                        Some((sin, cos)) => {
                            self.position.x.data -= distance * cos.data;
                            self.position.y.data += distance * sin.data;
                        }
                        _ => self.position.x.data += distance,
                    }
                }
            }
            Action::Backward { distance } => {
                if self.active {
                    match &self.camera_rotation {
                        Some((sin, cos)) => {
                            self.position.x.data += distance * cos.data;
                            self.position.y.data -= distance * sin.data;
                        }
                        _ => self.position.x.data -= distance,
                    }
                }
            }
            Action::Left { distance } => {
                if self.active {
                    match &self.camera_rotation {
                        Some((sin, cos)) => {
                            self.position.x.data -= distance * sin.data;
                            self.position.y.data -= distance * cos.data;
                        }
                        _ => self.position.y.data += distance,
                    }
                }
            }
            Action::Right { distance } => {
                if self.active {
                    match &self.camera_rotation {
                        Some((sin, cos)) => {
                            self.position.x.data += distance * sin.data;
                            self.position.y.data += distance * cos.data;
                        }
                        _ => self.position.y.data -= distance,
                    }
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
