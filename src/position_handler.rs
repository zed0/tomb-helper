//! Saving and restoring Lara's position, and flight mode ("active"), which holds her where she is, so the movement
//! actions can move her (relative to the camera, by `CameraSin`/`CameraCos`). Movement does nothing while it's off.
//!
//! The game moves her every frame (falling, and back out of walls), so while flying a thread of its own writes her
//! position every millisecond rather than once a tick of the main loop, which can be several frames apart with the
//! overlay drawing. Where the version has them, it also:
//! - zeroes her `Velocity`, or she's dragged down a little each frame by the speed she's built up falling;
//! - moves her `FallStart` with her, or she dies from the fall when she's flown down too far below where she was last
//!   on the ground;
//! - turns her proxy's collision off (`ProxyFlags`), or it's pushed back up out of the ground and she's pulled up after
//!   it. Its physics body stays where it was meanwhile, and once collision's back on only moves towards her, stopping
//!   at walls, so she'd go through walls without it: after the flight it's put back on her (`ProxyShapeCentre` and the
//!   rest);
//! - puts her proxy's `ProxyResolvedCentre` where its `ProxyCentre` is (where she is), as the game moves her back
//!   towards it, and it stays on this side of a wall she's flown through;
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
use std::time::{Duration, Instant};

const HOLD_INTERVAL: Duration = Duration::from_millis(1);
// How long her proxy's physics body is put back on her for after a flight
const BODY_RESET_TIME: Duration = Duration::from_millis(50);

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
            velocity: TrackedMemory::find(address_offsets, AddressType::Velocity, *arch, *base_addr),
            fall_start: [AddressType::FallStart, AddressType::FallStartCopy]
                .iter()
                .filter_map(|address_type| TrackedMemory::find(address_offsets, *address_type, *arch, *base_addr))
                .collect(),
            proxy_centre: TrackedMemory::find(address_offsets, AddressType::ProxyCentre, *arch, *base_addr),
            proxy_resolved: TrackedMemory::find(address_offsets, AddressType::ProxyResolvedCentre, *arch, *base_addr),
            proxy_flags: TrackedMemory::find(address_offsets, AddressType::ProxyFlags, *arch, *base_addr),
            body_centres: [AddressType::ProxyShapeCentre, AddressType::ProxyShapePreviousCentre]
                .iter()
                .filter_map(|address_type| TrackedMemory::find(address_offsets, *address_type, *arch, *base_addr))
                .collect(),
            body_gaps: [AddressType::ProxyShapeGap, AddressType::ProxyBodyGap]
                .iter()
                .filter_map(|address_type| TrackedMemory::find(address_offsets, *address_type, *arch, *base_addr))
                .collect(),
            camera_collision: TrackedMemory::find(address_offsets, AddressType::CameraCollision, *arch, *base_addr),
            ..Held::new(*handle, &position)
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
            held: Held::new(*handle, &position),
            position,
            camera_rotation: None,
            flight: None,
            handle: *handle,
            name: "look at position".to_string(),
            saved: false,
            is_lara: false,
        })
    }

    // Moves the cached position this far forward and right, relative to the camera if the version has it (else along x
    // and -y), while flying
    fn move_by(&mut self, forward: f32, right: f32) {
        if self.flight.is_none() {
            return;
        }
        let position = &mut self.position;
        match &self.camera_rotation {
            Some((sin, cos)) => {
                position.x.data += right * sin.data - forward * cos.data;
                position.y.data += forward * sin.data + right * cos.data;
            }
            None => {
                position.x.data += forward;
                position.y.data -= right;
            }
        }
    }

    // Moves the cached position up, while flying
    fn move_up(&mut self, up: f32) {
        if self.flight.is_some() {
            self.position.z.data += up;
        }
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
            Action::Forward { distance } => self.move_by(distance, 0.0),
            Action::Backward { distance } => self.move_by(-distance, 0.0),
            Action::Left { distance } => self.move_by(0.0, -distance),
            Action::Right { distance } => self.move_by(0.0, distance),
            Action::Up { distance } => self.move_up(distance),
            Action::Down { distance } => self.move_up(-distance),
            _ => (),
        }
        self.hold_here();
        Ok(())
    }
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
    proxy_resolved: Option<TrackedMemory<[f32; 3]>>,
    proxy_flags: Option<TrackedMemory<u32>>,
    // Whether it collided before the flight, to turn it back on after
    proxy_collided: Option<bool>,
    // Put where `proxy_centre` is after the flight
    body_centres: Vec<TrackedMemory<[f32; 3]>>,
    // Zeroed after the flight
    body_gaps: Vec<TrackedMemory<[f32; 4]>>,
    camera_collision: Option<TrackedMemory<u8>>,
    // What it was before the flight, put back after
    camera_collision_was: Option<u8>,
    // The bit of `proxy_flags` set while it collides
    proxy_collides: u32,
}

impl Held {
    // Holding just her position
    fn new(handle: ProcessHandle, position: &TrackedPosition) -> Held {
        Held {
            handle: SendHandle(handle),
            position: position.clone(),
            velocity: None,
            fall_start: vec![],
            proxy_centre: None,
            proxy_resolved: None,
            proxy_flags: None,
            proxy_collided: None,
            body_centres: vec![],
            body_gaps: vec![],
            camera_collision: None,
            camera_collision_was: None,
            proxy_collides: crate::layout::current().proxy.movement_collides as u32,
        }
    }

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
                self.proxy_collided.get_or_insert(flags.data & self.proxy_collides != 0);
                if flags.data & self.proxy_collides != 0 {
                    flags.data &= !self.proxy_collides;
                    let _ = flags.apply_to_game(handle);
                }
            }
        }
        if let (Some(centre), Some(resolved)) = (&mut self.proxy_centre, &mut self.proxy_resolved) {
            if centre.fetch_from_game(handle).is_ok() {
                resolved.data = centre.data;
                let _ = resolved.apply_to_game(handle);
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
        let handle = self.handle.0;
        if let (Some(flags), Some(true)) = (&mut self.proxy_flags, self.proxy_collided) {
            if flags.fetch_from_game(handle).is_ok() {
                flags.data |= self.proxy_collides;
                let _ = flags.apply_to_game(handle);
            }
            // Over a few frames, in case the game moves it first
            let started = Instant::now();
            while started.elapsed() < BODY_RESET_TIME {
                if let Some(centre) = &mut self.proxy_centre {
                    if centre.fetch_from_game(handle).is_ok() {
                        for body_centre in &mut self.body_centres {
                            body_centre.data = centre.data;
                            let _ = body_centre.apply_to_game(handle);
                        }
                    }
                }
                for gap in &mut self.body_gaps {
                    gap.data = [0.0; 4];
                    let _ = gap.apply_to_game(handle);
                }
                thread::sleep(HOLD_INTERVAL);
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
