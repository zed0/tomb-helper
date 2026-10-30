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
//! that's left for later. With `flight_skew` in the config (the default), it has the game do it instead: the game's
//! own noclip (`skew`, which turns her gravity and collision off, and back on afterwards) is on for the flight, and off
//! again when it ends, or when the handler goes (tomb-helper stopping, or the offsets reloading).
//!
//! Storing her position also stores her facing and the camera's yaw and pitch, where the version has them. Restoring
//! it outside a flight is a short one (`RESTORE_HOLD`): she's held at the stored position as in a flight (with skew,
//! and her velocity and fall start, so she doesn't die from a fall she didn't take), then let go. Her facing and the
//! camera are put back with her position, and again as she's let go. Written once in TR2013 Steam, both stayed where
//! they were put.

use crate::action::Action;
use crate::components::Components;
use crate::find_process::Connection;
use crate::game::Game;
use crate::handler::{Handler, HelperState};
use crate::position::Position;
use crate::skew::Skew;
use crate::process_details::AddressType;
use crate::targets::{self, Source};
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const HOLD_INTERVAL: Duration = Duration::from_millis(1);
// How long restoring her position (outside a flight) holds her there before letting go: about a frame, as the camera
// zooms out and back in while skew's on (200ms and 50ms showed it; 20ms looked best)
const RESTORE_HOLD: Duration = Duration::from_millis(20);

pub struct PositionHandler {
    // While flying, where she's held
    position: TrackedPosition,
    // What `position` is, which is checked before it's written (`targets::verify`)
    position_types: [AddressType; 3],
    saved_position: TrackedPosition,
    // Something's been stored in `saved_position`
    saved: bool,
    // Her facing and the camera's angles, stored with it
    saved_view: View,
    // While a restore holds her (in a flight of its own), until when
    restore_until: Option<Instant>,
    // Lara's (rather than the look at position), which the overlay shows, and what flight mode holds besides
    is_lara: bool,
    camera_rotation: Option<(TrackedMemory<f32>, TrackedMemory<f32>)>,
    // Set while flying
    flight: Option<Flight>,
    // The game's noclip, for flights (with `flight_skew`, where the version has it), and whether it's on
    skew: Option<Skew>,
    skewing: bool,
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
    pub fn new_position_handler(connection: &Connection, use_skew: bool) -> Option<PositionHandler> {
        println!("Loading position handler...");
        let mut handler = PositionHandler::new(connection, LARA, "position")?;
        handler.camera_rotation = TrackedMemory::find(connection, AddressType::CameraSin)
            .zip(TrackedMemory::find(connection, AddressType::CameraCos));
        handler.is_lara = true;
        handler.skew = if use_skew { Skew::find(connection) } else { None };
        Some(handler)
    }

    pub fn new_look_at_handler(connection: &Connection) -> Option<PositionHandler> {
        println!("Loading look at position handler...");
        PositionHandler::new(connection, LOOK_AT, "look at position")
    }

    // For the position at these offsets, if the version has them
    fn new(connection: &Connection, position_types: [AddressType; 3], name: &str) -> Option<PositionHandler> {
        let [x, y, z] = position_types.map(|t| connection.details.address_offsets.get(&t).cloned());
        let position = TrackedPosition::new(x?, y?, z?, connection.details.arch, connection.base_address);
        Some(PositionHandler {
            saved_position: position.clone(),
            position,
            position_types,
            saved: false,
            saved_view: View::default(),
            restore_until: None,
            is_lara: false,
            camera_rotation: None,
            flight: None,
            skew: None,
            skewing: false,
            game: connection.game(),
            components: Components::new(connection.base_address),
            arch: connection.details.arch,
            handle: connection.process,
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

    // What a flight writes besides her position: what's found and looks right (for Lara; nothing for the look at).
    // What's held is said for flights, not restores
    fn find_held(&mut self, announce: bool) -> Held {
        let mut held = Held::new(self.handle, &self.position);
        if !self.is_lara {
            return held;
        }
        let mut notes = Notes::default();
        let (arch, base) = (self.arch, self.game.base_address);
        let mut find = |address_type| notes.find(&self.game, &mut self.components, address_type);
        held.fall_start = [AddressType::FallStart, AddressType::FallStartCopy]
            .into_iter()
            .filter_map(&mut find)
            .map(|chain| tracked_at(chain, arch, base))
            .collect();
        held.velocity = find(AddressType::Velocity).map(|chain| tracked_at(chain, arch, base));
        if !announce {
            notes.held.clear();
        }
        notes.print("Flight mode holds", "Flight mode leaves alone, as it looks wrong");
        held
    }

    // Flying, rather than held by a restore
    fn flying(&self) -> bool {
        self.flight.is_some() && self.restore_until.is_none()
    }

    // Her facing and the camera's angles, those found that look right (for Lara; nothing for the look at)
    fn read_view(&mut self) -> View {
        if !self.is_lara {
            return View::default();
        }
        let mut notes = Notes::default();
        let view = View {
            facing: self.read_found(&mut notes, AddressType::Facing),
            camera_yaw: self.read_found(&mut notes, AddressType::CameraYaw),
            camera_pitch: self.read_found(&mut notes, AddressType::CameraPitch),
        };
        notes.print("Stored with it", "Not stored with it, as it looks wrong");
        view
    }

    fn read_found<T: Copy + Default + std::fmt::Debug>(&mut self, notes: &mut Notes, address_type: AddressType) -> Option<T> {
        let chain = notes.find(&self.game, &mut self.components, address_type)?;
        let mut value = tracked_at::<T>(chain, self.arch, self.game.base_address);
        value.fetch_from_game(self.handle).ok()?;
        Some(value.data)
    }

    // Puts back the stored facing and camera angles, where they're found now
    fn write_view(&mut self) {
        let view = self.saved_view;
        let mut notes = Notes::default();
        if let Some(facing) = view.facing {
            self.write_found(&mut notes, AddressType::Facing, facing);
        }
        if let Some(yaw) = view.camera_yaw {
            self.write_found(&mut notes, AddressType::CameraYaw, yaw);
        }
        if let Some(pitch) = view.camera_pitch {
            self.write_found(&mut notes, AddressType::CameraPitch, pitch);
        }
        notes.held.clear();
        notes.print("", "Not restoring it, as it looks wrong");
    }

    fn write_found<T: Copy + std::fmt::Debug>(&mut self, notes: &mut Notes, address_type: AddressType, value: T) {
        if let Some(chain) = notes.find(&self.game, &mut self.components, address_type) {
            if let Err(e) = TrackedMemory::new(value, chain, self.arch, self.game.base_address).apply_to_game(self.handle) {
                eprintln!("Error: couldn't restore {:?}: {}", address_type, e);
            }
        }
    }

    // Lets her go after a restore's hold, and puts back her facing and the camera
    fn end_restore(&mut self) {
        self.restore_until = None;
        self.flight = None;
        self.stop_skew(false);
        self.write_view();
    }

    // Moves the cached position this far forward and right, relative to the camera if the version has it (else along x
    // and -y), while flying
    fn move_by(&mut self, forward: f32, right: f32) {
        if !self.flying() {
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
        if self.flying() {
            self.position.z.data += up;
        }
    }

    // The game's noclip for a flight (or a restore's hold), if it's to be used; it's said for flights
    fn start_skew(&mut self, announce: bool) {
        if let Some(skew) = &self.skew {
            match skew.set(&self.game, &mut self.components, true) {
                Ok(()) => {
                    self.skewing = true;
                    if announce {
                        println!("Flight mode turned on the game's skew (no gravity or collision)");
                    }
                }
                Err(why) => {
                    eprintln!("Error: flight mode couldn't turn on the game's skew, so isn't using it: {}", why)
                }
            }
        }
    }

    // After a flight, if it was on
    fn stop_skew(&mut self, announce: bool) {
        if !self.skewing {
            return;
        }
        self.skewing = false;
        if let Some(skew) = &self.skew {
            match skew.set(&self.game, &mut self.components, false) {
                Ok(()) if announce => println!("Flight mode turned off the game's skew"),
                Ok(()) => (),
                Err(why) => eprintln!("Error: flight mode couldn't turn off the game's skew: {}", why),
            }
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
            state.flying = self.flying();
        }
    }

    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if self.restore_until.is_some_and(|until| Instant::now() >= until) {
            self.end_restore();
        }
        if self.flying() {
            if let Some((sin, cos)) = &mut self.camera_rotation {
                sin.fetch_from_game(self.handle)?;
                cos.fetch_from_game(self.handle)?;
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        let active = self.flying();
        match action {
            Action::ToggleActive {} => {
                if self.restore_until.take().is_some() {
                    // A restore's hold carries on as a flight
                    self.write_view();
                    println!("Activated {} handler", self.name);
                } else if active {
                    self.flight = None;
                    self.stop_skew(true);
                    println!("Deactivated {} handler", self.name);
                } else {
                    // Nothing's held unless her position reads, and looks like hers
                    if let Err(e) = self.position.fetch_from_game(self.handle) {
                        return Err(format!("Not activating the {} handler, its position couldn't be read: {}", self.name, e).into());
                    }
                    if let Err(why) = self.check_position() {
                        return Err(format!("Not activating the {} handler, its position looks wrong ({})", self.name, why).into());
                    }
                    let held = self.find_held(true);
                    self.start_skew(true);
                    self.flight = Some(Flight::start(held, self.position.to_position()));
                    println!("Activated {} handler", self.name);
                }
            }
            Action::StorePosition {} => {
                self.saved_position = self.position.clone();
                self.saved_position.fetch_from_game(self.handle)?;
                self.saved = true;
                println!("Stored {}! {:}", self.name, self.saved_position);
                self.saved_view = self.read_view();
            }
            Action::RestorePosition {} => {
                if !self.saved {
                    println!("No {} stored to restore", self.name);
                    return Ok(());
                }
                if let Err(why) = self.check_position() {
                    return Err(format!("Not restoring the {}, it looks wrong ({})", self.name, why).into());
                }
                self.position = self.saved_position.clone();
                // A flight goes on, moved there below; otherwise she's held there for a moment
                if !self.flying() {
                    if self.restore_until.is_none() {
                        let held = self.find_held(false);
                        self.start_skew(false);
                        self.flight = Some(Flight::start(held, self.position.to_position()));
                    }
                    self.restore_until = Some(Instant::now() + RESTORE_HOLD);
                }
                // With her position, so it all changes at once (and again when the hold ends, in case it's undone)
                self.write_view();
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

// A flight that's still on when the handler goes ends, with the game's noclip
impl Drop for PositionHandler {
    fn drop(&mut self) {
        self.flight = None;
        self.stop_skew(true);
    }
}

// What's stored with her position, where it's found
#[derive(Debug, Clone, Copy, Default)]
struct View {
    // Her instance's Euler angles
    facing: Option<[f32; 3]>,
    // The camera controller's, each with its copy
    camera_yaw: Option<[f32; 2]>,
    camera_pitch: Option<[f32; 2]>,
}

// The value at the end of a chain, as the type it's held as
fn tracked_at<T: Copy + Default + std::fmt::Debug>(chain: Vec<usize>, arch: Architecture, base: usize) -> TrackedMemory<T> {
    TrackedMemory::new(T::default(), chain, arch, base)
}

// What was found to write, and what wasn't, said once
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

    fn print(&self, held: &str, skipped: &str) {
        if !self.held.is_empty() {
            println!("{}: {}", held, self.held.join(", "));
        }
        for what in &self.skipped {
            println!("{}: {}", skipped, what);
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
