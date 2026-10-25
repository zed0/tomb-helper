//! Saving and restoring Lara's position, and flight mode ("active"), which holds her where she is, so the movement
//! actions can move her (relative to the camera, by `CameraSin`/`CameraCos`). Movement does nothing while it's off.
//!
//! The game moves her every frame (falling, and back out of walls), so while flying a thread of its own writes her
//! position every millisecond rather than once a tick of the main loop, which can be several frames apart with the
//! overlay drawing. Where the version has them, it also:
//! - zeroes her `Velocity`, or she's dragged down a little each frame by the speed she's built up falling;
//! - moves her `FallStart` with her, or she dies from the fall when she's flown down too far below where she was last
//!   on the ground;
//! - turns her proxy's collision off (`ProxyFlags`, back on after), or it's pushed back up out of the ground and she's
//!   pulled up after it;
//! - puts her proxy's `ProxyResolvedCentre` and `ProxyBodyCentre` where its `ProxyCentre` is (where she is), as the
//!   game moves her back towards them, and they stay on this side of a wall she's flown through;
//! - turns the camera's `CameraCollision` off (back to what it was after), or it stays on this side of the wall.

use crate::action::Action;
use crate::handler::{Handler, HelperState};
use crate::position::Position;
use crate::process_details::{AddressOffsets, AddressType};
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const HOLD_INTERVAL: Duration = Duration::from_millis(1);
// Set in `ProxyFlags` while her proxy collides (SOTTR)
const PROXY_COLLIDES: u32 = 0x1;

pub struct PositionHandler {
    // While flying, where she's held
    position: TrackedPosition,
    saved_position: TrackedPosition,
    // Something's been stored in `saved_position`
    saved: bool,
    // Lara's (rather than the look at position), which the overlay shows
    is_lara: bool,
    camera_rotation: Option<(TrackedMemory<f32>, TrackedMemory<f32>)>,
    // What each flight writes, but for where she's held
    held: Held,
    // Set while flying
    flight: Option<Flight>,
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

        let position = TrackedPosition::new(
            address_offsets.get(&AddressType::XPosition)?.clone(),
            address_offsets.get(&AddressType::YPosition)?.clone(),
            address_offsets.get(&AddressType::ZPosition)?.clone(),
            *arch,
            *base_addr,
        );
        let held = Held {
            handle: SendHandle(*handle),
            position: position.clone(),
            velocity: tracked(address_offsets, &AddressType::Velocity, arch, base_addr),
            fall_start: [AddressType::FallStart, AddressType::FallStartCopy]
                .iter()
                .filter_map(|address_type| tracked(address_offsets, address_type, arch, base_addr))
                .collect(),
            proxy_centre: tracked(address_offsets, &AddressType::ProxyCentre, arch, base_addr),
            proxy_followers: [AddressType::ProxyResolvedCentre, AddressType::ProxyBodyCentre]
                .iter()
                .filter_map(|address_type| tracked(address_offsets, address_type, arch, base_addr))
                .collect(),
            proxy_flags: tracked(address_offsets, &AddressType::ProxyFlags, arch, base_addr),
            proxy_collided: None,
            camera_collision: tracked(address_offsets, &AddressType::CameraCollision, arch, base_addr),
            camera_collision_was: None,
        };
        Some(PositionHandler {
            saved_position: position.clone(),
            position,
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
            held,
            flight: None,
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

        let position = TrackedPosition::new(
            address_offsets.get(&AddressType::XLookAt)?.clone(),
            address_offsets.get(&AddressType::YLookAt)?.clone(),
            address_offsets.get(&AddressType::ZLookAt)?.clone(),
            *arch,
            *base_addr,
        );
        Some(PositionHandler {
            saved_position: position.clone(),
            held: Held {
                handle: SendHandle(*handle),
                position: position.clone(),
                velocity: None,
                fall_start: vec![],
                proxy_centre: None,
                proxy_followers: vec![],
                proxy_flags: None,
                proxy_collided: None,
                camera_collision: None,
                camera_collision_was: None,
            },
            position,
            camera_rotation: None,
            flight: None,
            handle: *handle,
            name: "look at position".to_string(),
            saved: false,
            is_lara: false,
        })
    }

    // Moves where she's held to the cached position
    fn hold_here(&self) {
        if let Some(flight) = &self.flight {
            *flight.target.lock().unwrap() = self.position.to_position();
        }
    }
}

impl Handler for PositionHandler {
    fn report(&self, state: &mut HelperState) {
        if self.is_lara {
            state.position_tools = true;
            state.saved_position = if self.saved { Some(self.saved_position.to_position()) } else { None };
            state.flying = self.flight.is_some();
        }
    }

    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if self.flight.is_some() {
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
        let active = self.flight.is_some();
        match action {
            Action::ToggleActive {} => {
                if active {
                    self.flight = None;
                    println!("Deactivated {} handler", self.name);
                } else {
                    match self.position.fetch_from_game(self.handle) {
                        Err(msg) => eprintln!("Error activating {} handler: {}", self.name, msg),
                        Ok(()) => println!("Activated {} handler", self.name),
                    }
                    self.flight = Some(Flight::start(self.held.clone(), self.position.to_position()));
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
                if active {
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
                if active {
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
                if active {
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
                if active {
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
                if active {
                    self.position.z.data += distance;
                }
            }
            Action::Down { distance } => {
                if active {
                    self.position.z.data -= distance;
                }
            }
            _ => (),
        }
        self.hold_here();
        Ok(())
    }
}

// The value at `address_type`, if this version has it
fn tracked<T: Copy + Default + std::fmt::Debug>(
    address_offsets: &AddressOffsets,
    address_type: &AddressType,
    arch: &Architecture,
    base_addr: &usize,
) -> Option<TrackedMemory<T>> {
    address_offsets
        .get(address_type)
        .map(|offsets| TrackedMemory::new(T::default(), offsets.clone(), *arch, *base_addr))
}

// Process handles can be used from any thread
#[derive(Clone, Copy)]
struct SendHandle(ProcessHandle);
unsafe impl Send for SendHandle {}

// What the flight thread writes. Each is written even if the others fail (they're unreadable while the game's loading,
// and there's nothing to do about it then)
#[derive(Clone)]
struct Held {
    handle: SendHandle,
    position: TrackedPosition,
    velocity: Option<TrackedMemory<[f32; 3]>>,
    fall_start: Vec<TrackedMemory<f32>>,
    proxy_centre: Option<TrackedMemory<[f32; 3]>>,
    // Put where `proxy_centre` is
    proxy_followers: Vec<TrackedMemory<[f32; 3]>>,
    proxy_flags: Option<TrackedMemory<u32>>,
    // Whether it collided before the flight, to turn it back on after
    proxy_collided: Option<bool>,
    camera_collision: Option<TrackedMemory<u8>>,
    // What it was before the flight, put back after
    camera_collision_was: Option<u8>,
}

impl Held {
    fn hold(&mut self, at: Position) {
        let handle = self.handle.0;
        self.position.x.data = at.x;
        self.position.y.data = at.y;
        self.position.z.data = at.z;
        let _ = self.position.apply_to_game(handle);
        if let Some(velocity) = &mut self.velocity {
            velocity.data = [0.0; 3];
            let _ = velocity.apply_to_game(handle);
        }
        for fall_start in &mut self.fall_start {
            fall_start.data = at.z;
            let _ = fall_start.apply_to_game(handle);
        }
        if let Some(flags) = &mut self.proxy_flags {
            if flags.fetch_from_game(handle).is_ok() {
                self.proxy_collided.get_or_insert(flags.data & PROXY_COLLIDES != 0);
                if flags.data & PROXY_COLLIDES != 0 {
                    flags.data &= !PROXY_COLLIDES;
                    let _ = flags.apply_to_game(handle);
                }
            }
        }
        if let Some(centre) = &mut self.proxy_centre {
            if centre.fetch_from_game(handle).is_ok() {
                for follower in &mut self.proxy_followers {
                    follower.data = centre.data;
                    let _ = follower.apply_to_game(handle);
                }
            }
        }
        if let Some(collision) = &mut self.camera_collision {
            if self.camera_collision_was.is_none() && collision.fetch_from_game(handle).is_ok() {
                self.camera_collision_was = Some(collision.data);
            }
            collision.data = 0;
            let _ = collision.apply_to_game(handle);
        }
    }

    fn release(&mut self) {
        if let (Some(flags), Some(true)) = (&mut self.proxy_flags, self.proxy_collided) {
            if flags.fetch_from_game(self.handle.0).is_ok() {
                flags.data |= PROXY_COLLIDES;
                let _ = flags.apply_to_game(self.handle.0);
            }
        }
        if let (Some(collision), Some(was)) = (&mut self.camera_collision, self.camera_collision_was) {
            collision.data = was;
            let _ = collision.apply_to_game(self.handle.0);
        }
    }
}

// The thread holding her while flying, stopped when this is dropped
struct Flight {
    target: Arc<Mutex<Position>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Flight {
    fn start(mut held: Held, at: Position) -> Flight {
        let target = Arc::new(Mutex::new(at));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (target, stop) = (target.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let at = *target.lock().unwrap();
                    held.hold(at);
                    thread::sleep(HOLD_INTERVAL);
                }
                held.release();
            })
        };
        Flight { target, stop, thread: Some(thread) }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
