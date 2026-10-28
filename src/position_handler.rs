//! Saving and restoring Lara's position, and flight mode ("active"), which holds her where she is, so the movement
//! actions can move her (relative to the camera, by `CameraSin`/`CameraCos`). Movement does nothing while it's off.
//!
//! The game moves her every frame (falling, and back out of walls), so while flying a thread of its own writes her
//! position every millisecond rather than once a tick of the main loop, which can be several frames apart with the
//! overlay drawing. Where the version has them, it also:
//! - zeroes her `Velocity`, or she's dragged down a little each frame by the speed she's built up falling;
//! - moves her `FallStart` with her, or she dies from the fall when she's flown down too far below where she was last
//!   on the ground;
//!
//! It doesn't touch collision, hers or the camera's, so she can't be flown through walls or into the ground. Turning
//! her proxy's off (`ProxyFlags`, with `ProxyResolvedCentre` and the camera's `CameraCollision`, which `targets` still
//! finds) left her physics body behind and broke her collision after the flight (she could jump through walls), so
//! that's left for later.

use crate::action::Action;
use crate::components::Components;
use crate::game::Game;
use crate::game_memory::Handle as GameHandle;
use crate::handler::{Handler, HelperState};
use crate::position::Position;
use crate::process_details::{AddressOffsets, AddressType};
use crate::targets::{self, Source};
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use process_memory::{Architecture, Pid, ProcessHandle};
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const HOLD_INTERVAL: Duration = Duration::from_millis(1);

pub struct PositionHandler {
    // While flying, where she's held
    position: TrackedPosition,
    // What `position` is, which is checked before it's written (`targets::verify`)
    position_types: [AddressType; 3],
    saved_position: TrackedPosition,
    // Something's been stored in `saved_position`
    saved: bool,
    // Lara's (rather than the look at position), which the overlay shows, and what flight mode holds besides
    is_lara: bool,
    camera_rotation: Option<(TrackedMemory<f32>, TrackedMemory<f32>)>,
    // Set while flying
    flight: Option<Flight>,
    // For finding and checking what flight mode writes
    game: Game,
    components: Components,
    arch: Architecture,
    handle: ProcessHandle,
    name: String,
}

const LARA: [AddressType; 3] = [AddressType::XPosition, AddressType::YPosition, AddressType::ZPosition];
const LOOK_AT: [AddressType; 3] = [AddressType::XLookAt, AddressType::YLookAt, AddressType::ZLookAt];

impl PositionHandler {
    pub fn new_position_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
    ) -> Option<PositionHandler> {
        println!("Loading position handler...");
        let camera_rotation = match (address_offsets.get(&AddressType::CameraSin), address_offsets.get(&AddressType::CameraCos)) {
            (Some(sin), Some(cos)) => Some((
                TrackedMemory::new(0.0, sin.clone(), *arch, *base_addr),
                TrackedMemory::new(0.0, cos.clone(), *arch, *base_addr),
            )),
            _ => None,
        };
        let handler = PositionHandler::new(address_offsets, arch, base_addr, handle, pid, LARA, "position")?;
        Some(PositionHandler { camera_rotation, is_lara: true, ..handler })
    }

    pub fn new_look_at_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
    ) -> Option<PositionHandler> {
        println!("Loading look at position handler...");
        PositionHandler::new(address_offsets, arch, base_addr, handle, pid, LOOK_AT, "look at position")
    }

    // For the position at these offsets, if the version has them
    fn new(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
        position_types: [AddressType; 3],
        name: &str,
    ) -> Option<PositionHandler> {
        let [x, y, z] = position_types.map(|t| address_offsets.get(&t).cloned());
        let position = TrackedPosition::new(x?, y?, z?, *arch, *base_addr);
        Some(PositionHandler {
            saved_position: position.clone(),
            position,
            position_types,
            saved: false,
            is_lara: false,
            camera_rotation: None,
            flight: None,
            game: Game::new(GameHandle::Live(*pid, *handle), *base_addr, address_offsets),
            components: Components::new(*base_addr),
            arch: *arch,
            handle: *handle,
            name: name.to_string(),
        })
    }

    // Whether the position's chains lead to a position, rather than somewhere else in a build they're wrong for
    fn check_position(&mut self) -> Result<(), String> {
        for address_type in self.position_types {
            targets::check(&self.game, &mut self.components, address_type).map_err(|why| format!("{:?}: {}", address_type, why))?;
        }
        Ok(())
    }

    // What a flight writes besides her position: what's found and looks right (for Lara; nothing for the look at)
    fn find_held(&mut self) -> Held {
        let mut held = Held::new(self.handle, &self.position);
        if !self.is_lara {
            return held;
        }
        let mut notes = Notes::default();
        let (game, components) = (&self.game, &mut self.components);
        let (arch, base) = (self.arch, self.game.base_address);
        let mut find = |address_type| notes.find(game, components, address_type);
        let mut find_all = |types: [AddressType; 2]| -> Vec<Vec<usize>> { types.iter().filter_map(|t| find(*t)).collect() };
        let fall_start = find_all([AddressType::FallStart, AddressType::FallStartCopy]);
        held.fall_start = fall_start.into_iter().map(|c| tracked_at(c, arch, base)).collect();
        let mut find = |address_type| notes.find(game, components, address_type);
        held.velocity = find(AddressType::Velocity).map(|c| tracked_at(c, arch, base));
        notes.print();
        held
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
                    // Nothing's held unless her position reads, and looks like hers
                    if let Err(e) = self.position.fetch_from_game(self.handle) {
                        return Err(format!("Not activating the {} handler, its position couldn't be read: {}", self.name, e).into());
                    }
                    if let Err(why) = self.check_position() {
                        return Err(format!("Not activating the {} handler, its position looks wrong ({})", self.name, why).into());
                    }
                    let held = self.find_held();
                    self.flight = Some(Flight::start(held, self.position.to_position()));
                    println!("Activated {} handler", self.name);
                }
            }
            Action::StorePosition {} => {
                self.saved_position = self.position.clone();
                self.saved_position.fetch_from_game(self.handle)?;
                self.saved = true;
                println!("Stored {}! {:}", self.name, self.saved_position);
            }
            Action::RestorePosition {} => {
                if let Err(why) = self.check_position() {
                    return Err(format!("Not restoring the {}, it looks wrong ({})", self.name, why).into());
                }
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

// The value at the end of a chain, as the type it's held as
fn tracked_at<T: Copy + Default + std::fmt::Debug>(chain: Vec<usize>, arch: Architecture, base: usize) -> TrackedMemory<T> {
    TrackedMemory::new(T::default(), chain, arch, base)
}

// What a flight found to hold, and what it didn't, said once as it starts
#[derive(Default)]
struct Notes {
    held: Vec<String>,
    skipped: Vec<String>,
}

impl Notes {
    // The value's chain, if one is found that looks right (`targets::find`)
    fn find(&mut self, game: &Game, components: &mut Components, address_type: AddressType) -> Option<Vec<usize>> {
        // Ones the version has no way to find aren't mentioned
        if targets::candidates(game, components, address_type).is_empty() {
            return None;
        }
        match targets::find(game, components, address_type) {
            Ok((chain, source)) => {
                let through = if source == Source::Structure { " (through the structures)" } else { "" };
                self.held.push(format!("{:?}{}", address_type, through));
                Some(chain)
            }
            Err(why) => {
                self.skipped.push(format!("{:?}: {}", address_type, why));
                None
            }
        }
    }

    fn print(&self) {
        if !self.held.is_empty() {
            println!("Flight mode holds: {}", self.held.join(", "));
        }
        for skipped in &self.skipped {
            println!("Flight mode leaves alone, as it looks wrong: {}", skipped);
        }
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
}

impl Held {
    // Holding just her position
    fn new(handle: ProcessHandle, position: &TrackedPosition) -> Held {
        Held {
            handle: SendHandle(handle),
            position: position.clone(),
            velocity: None,
            fall_start: vec![],
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
