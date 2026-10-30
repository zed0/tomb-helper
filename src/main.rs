//! tomb-helper: finds the running game, builds the handlers for its version, and passes them hotkey actions and ticks.
//! With any arguments it runs a command line tool instead (`cli`).
//!
//! It polls for a known game every 100ms; once one's running it creates the handlers (`create_handlers`; each is left
//! out if the version lacks the offsets it needs), registers the hotkeys (one key can trigger several actions), then
//! loops every 5ms: actions to every handler, then a tick for each. Every 500ms it checks the offsets file, recreating
//! the handlers if the running version's offsets changed. It disconnects when the game exits.

// The tools' results are often tuples of what they found, which are clearer spelled out where they're used
#![allow(clippy::type_complexity)]

use crate::action::Action;
use crate::config::{Config, CutsceneTiming, Hotkey};
use crate::cutscene_handler::CutsceneHandler;
use crate::cutscene_timing_generator_handler::CutsceneTimingGeneratorHandler;
use crate::find_process::Connection;
use crate::handler::{Handler, HelperState};
use crate::hotkeys::{Callback, Hotkeys, KeyCode};
use crate::menu_handler::MenuHandler;
use crate::offsets_file::OffsetsSource;
use crate::overlay_handler::OverlayHandler;
use crate::position_handler::PositionHandler;
use crate::process_details::ProcessDetails;
use signal_hook::consts::SIGINT;
use signal_hook::flag;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::{thread, time};

// First, so its `println!` and the rest, which log too, are the ones every module after it uses
#[macro_use]
mod logging;
mod action;
mod aim_sweep;
mod camera_fields;
mod chains;
mod check;
mod class_fields;
mod cli;
mod code_trace;
mod collision;
mod collision_mesh;
mod components;
mod config;
mod cutscene_handler;
mod cutscene_id;
mod cutscene_timing_generator_handler;
mod cutscene_timing_info;
mod derive;
mod discover;
mod fall_start;
mod find_process;
mod fit_layout;
mod flight_fields;
mod game;
mod game_memory;
mod handler;
mod hotkeys;
mod inject32;
mod layout;
mod menu;
mod menu_handler;
mod normalised;
mod object_list;
mod offsets_file;
mod overlay_handler;
mod pe_image;
mod port;
mod position;
mod position_handler;
mod process_details;
mod readable_from_path;
mod rtti;
mod snapshot;
mod targets;
mod tracked_memory;
mod tracked_position;
mod trigger;
mod trigger_scripts;
mod update_offsets;
mod value_layout;
mod watch;
mod watch_fields;
mod words;
// Shared with the overlay, which uses the other half of it
#[allow(dead_code)]
mod ui_ipc_data;
// Shared with the overlay, which draws with it
#[allow(dead_code)]
mod projection;

#[cfg(windows)]
extern crate winapi;

// How often to look for a game while none is running
const SEARCH_INTERVAL: time::Duration = time::Duration::from_millis(100);
// How often to check whether the offsets file has changed while connected
const OFFSETS_CHECK_INTERVAL: time::Duration = time::Duration::from_millis(500);
// How often the handlers tick while connected (the hotkeys are polled as often), rather than as fast as they can
const TICK_INTERVAL: time::Duration = time::Duration::from_millis(5);

fn main() {
    logging::log_panics();
    let args: Vec<String> = std::env::args().skip(1).collect();
    logging::write(logging::Stream::Out, &format!("tomb-helper {} started {:?}\n", env!("CARGO_PKG_VERSION"), args));
    if !args.is_empty() {
        if let Err(e) = cli::run() {
            eprintln!("{:#}", e);
            std::process::exit(1);
        }
        return;
    }

    let term = Arc::new(AtomicBool::new(false));

    flag::register(SIGINT, Arc::clone(&term)).expect("Could not register SIGINT handler");
    let config = config::get_config();
    print_help(&config.hotkeys);
    let mut offsets = OffsetsSource::new();
    let mut known = load_offsets(&mut offsets, vec![]);
    println!("Searching for Tomb Raider processes...");
    loop {
        if term.load(Ordering::Relaxed) {
            println!("Exiting");
            break;
        }

        // A version added to the offsets file is picked up straight away
        if offsets.changed() {
            known = load_offsets(&mut offsets, known);
        }

        if let Some(mut connection) = find_process::find_process(known.clone(), None) {
            connect(&config, &mut connection, &mut offsets, &mut known, &term);
            // The handlers, which had copies of it, are gone
            find_process::close_handle(connection.process);
            println!("Searching for Tomb Raider processes...");
        }

        thread::sleep(SEARCH_INTERVAL);
    }
}

/// Loads the offsets, keeping `current` if the file can't be loaded (e.g. it's half edited)
fn load_offsets(offsets: &mut OffsetsSource, current: Vec<ProcessDetails>) -> Vec<ProcessDetails> {
    match offsets.load() {
        Ok(known) => {
            println!("Loaded offsets for {} versions from {}", known.len(), offsets.describe());
            known
        }
        Err(e) => {
            eprintln!("Error: {:#}", e);
            eprintln!("Keeping the previous offsets until the file is fixed");
            current
        }
    }
}

fn create_handlers(config: &Config, connection: &Connection) -> Vec<Box<dyn Handler>> {
    fn boxed<H: Handler + 'static>(handler: H) -> Box<dyn Handler> {
        Box::new(handler)
    }

    // Each is None if the version doesn't have the offsets it needs
    let cutscenes = match &config.record_cutscene_timing {
        CutsceneTiming::On { timing_file, livesplit_port } => {
            CutsceneTimingGeneratorHandler::new(connection, timing_file, *livesplit_port).map(boxed)
        }
        CutsceneTiming::Off {} => {
            CutsceneHandler::new(connection, &config.cutscene_blacklist_file, &config.cutscene_timing_file).map(boxed)
        }
    };
    [
        cutscenes,
        OverlayHandler::new_overlay_handler(connection, &config.overlay, &config.hotkeys).map(boxed),
        PositionHandler::new_position_handler(connection).map(boxed),
        PositionHandler::new_look_at_handler(connection).map(boxed),
        MenuHandler::new(connection).map(boxed),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn connect(
    config: &Config,
    connection: &mut Connection,
    offsets: &mut OffsetsSource,
    known: &mut Vec<ProcessDetails>,
    term: &Arc<AtomicBool>,
) {
    let pid = connection.pid;
    println!("Connecting to {} with PID {}", connection.details, pid);

    let mut handlers = create_handlers(config, connection);
    let (tx, rx) = mpsc::channel();
    // Polls the keys until it's dropped, when this returns
    let _hotkeys = Hotkeys::start(hotkey_callbacks(&config.hotkeys, tx));
    println!("Started!");

    let mut last_offsets_check = time::Instant::now();
    loop {
        let tick_start = time::Instant::now();
        if term.load(Ordering::Relaxed) {
            return;
        }

        if !find_process::is_process_running(pid) {
            println!("Disconnected from {} with PID {}", connection.details, pid);
            return;
        }

        if last_offsets_check.elapsed() > OFFSETS_CHECK_INTERVAL {
            last_offsets_check = time::Instant::now();
            match reload_offsets(connection, offsets, known) {
                Reload::Same => {}
                Reload::Changed(details) => {
                    // Drop the old handlers first, so e.g. the overlay is ejected before it's re-injected
                    handlers.clear();
                    connection.details = *details;
                    handlers = create_handlers(config, connection);
                    println!("Reloaded offsets for {}", connection.details);
                }
                Reload::Gone => {
                    println!("{} no longer matches any offsets, disconnecting", connection.details);
                    return;
                }
            }
        }

        while let Ok(action) = rx.try_recv() {
            dispatch(&mut handlers, action, config.debug_logging);
        }
        tick(&mut handlers, config.debug_logging);
        thread::sleep(TICK_INTERVAL.saturating_sub(tick_start.elapsed()));
    }
}

// What a change to the offsets file means for the game connected to
enum Reload {
    // Nothing its handlers use changed (recreating them loses their state, and re-downloads cutscene info)
    Same,
    Changed(Box<ProcessDetails>),
    // It isn't a known version any more
    Gone,
}

// Loads the offsets file again if it's changed, and says whether the connected game's entry has
fn reload_offsets(connection: &Connection, offsets: &mut OffsetsSource, known: &mut Vec<ProcessDetails>) -> Reload {
    if !offsets.changed() {
        return Reload::Same;
    }
    *known = load_offsets(offsets, known.clone());
    let Some(found) = find_process::find_process(known.clone(), None) else {
        return Reload::Gone;
    };
    // Only its details are wanted
    find_process::close_handle(found.process);
    let details = &connection.details;
    if found.pid != connection.pid {
        Reload::Gone
    } else if found.details.version.version != details.version.version || found.details.address_offsets != details.address_offsets {
        Reload::Changed(Box::new(found.details))
    } else {
        Reload::Same
    }
}

// Sends each key's actions down `tx` when it's pressed, and while it's held those that repeat
fn hotkey_callbacks(hotkeys: &[Hotkey], tx: mpsc::Sender<Action>) -> HashMap<KeyCode, Callback> {
    let mut actions: HashMap<KeyCode, Vec<Action>> = HashMap::new();
    for hotkey in hotkeys {
        actions.entry(hotkey.key).or_default().push(hotkey.action);
    }
    actions
        .into_iter()
        .map(|(key, actions)| {
            let tx = tx.clone();
            let callback: Callback = Box::new(move |repeated| {
                for action in actions.iter().filter(|action| !repeated || action.repeats()) {
                    let _ = tx.send(*action);
                }
            });
            (key, callback)
        })
        .collect()
}

// Every handler's tick, then the overlay shows the other handlers' state, and its buttons ask for actions
fn tick(handlers: &mut [Box<dyn Handler>], debug_logging: bool) {
    for handler in handlers.iter_mut() {
        if let Err(e) = handler.handle_tick() {
            report(&*e, debug_logging);
        }
    }
    let mut state = HelperState::default();
    for handler in handlers.iter() {
        handler.report(&mut state);
    }
    let mut requested = vec![];
    for handler in handlers.iter_mut() {
        handler.show(&state);
        requested.extend(handler.take_actions());
    }
    for action in requested {
        dispatch(handlers, action, debug_logging);
    }
}

fn dispatch(handlers: &mut [Box<dyn Handler>], action: Action, debug_logging: bool) {
    for handler in handlers {
        if let Err(e) = handler.handle_action(action) {
            report(&*e, debug_logging);
        }
    }
}

// A handler's error. The game being unreadable (pointer chains that don't resolve, reads that fail) is expected while it
// loads a level or exits, so that's only shown with `debug_logging`.
fn report(error: &(dyn std::error::Error + 'static), debug_logging: bool) {
    if !game_memory::is_unreadable(error) {
        eprintln!("Error: {}", error);
    } else if debug_logging {
        eprintln!("Debug: {}", error);
    }
}

fn print_help(hotkeys: &[Hotkey]) {
    for hotkey in hotkeys {
        println!("{:?} => {:?}", hotkey.key, hotkey.action);
    }
}
