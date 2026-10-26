//! tomb-helper: finds the running game, builds the handlers for its version, and passes them hotkey actions and ticks.
//! With any arguments it runs a command line tool instead (`cli`).
//!
//! It polls for a known game every 100ms; once one's running it creates the handlers (`create_handlers`; each is left
//! out if the version lacks the offsets it needs), registers the hotkeys (one key can trigger several actions), then
//! loops: actions to every handler, then a tick for each. Every 500ms it checks the offsets file, recreating the
//! handlers if the running version's offsets changed. It disconnects when the game exits.

use crate::action::Action;
use crate::config::{Hotkey, CutsceneTiming, Config};
use crate::handler::{Handler, HelperState};
use crate::cutscene_handler::CutsceneHandler;
use crate::cutscene_timing_generator_handler::CutsceneTimingGeneratorHandler;
use crate::overlay_handler::OverlayHandler;
use crate::offsets_file::OffsetsSource;
use crate::position_handler::PositionHandler;
use crate::process_details::ProcessDetails;
use process_memory::Pid;
use itertools::Itertools;
use crate::hotkeys::{Callback, Hotkeys, KeyCode};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{thread, time};
use signal_hook::consts::SIGINT;
use signal_hook::flag;

mod action;
mod config;
mod cutscene_handler;
mod cutscene_id;
mod cutscene_timing_generator_handler;
mod camera_fields;
mod chains;
mod check;
mod class_fields;
mod cli;
mod collision;
mod collision_mesh;
mod components;
mod code_trace;
mod derive;
mod normalised;
mod discover;
mod pe_image;
mod position_handler;
mod handler;
mod hotkeys;
mod aim_sweep;
mod inject32;
mod layout;
mod layout_witness;
mod object_list;
mod overlay_handler;
mod port;
mod position;
mod process_details;
mod fall_start;
mod find_process;
mod fit_layout;
mod game;
mod game_memory;
mod offsets_file;
mod tracked_memory;
mod watch;
mod witness;
mod words;
mod tracked_position;
mod trigger;
mod trigger_scripts;
mod update_offsets;
mod value_layout;
mod cutscene_timing_info;
mod readable_from_path;
mod rtti;
mod snapshot;
mod targets;
// Shared with the overlay, which uses the other half of it
#[allow(dead_code)]
mod ui_ipc_data;

#[cfg(windows)]
extern crate winapi;

// How often to look for a game while none is running
const SEARCH_INTERVAL: time::Duration = time::Duration::from_millis(100);
// How often to check whether the offsets file has changed while connected
const OFFSETS_CHECK_INTERVAL: time::Duration = time::Duration::from_millis(500);

fn main() {
    if std::env::args().len() > 1 {
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

        if let Some((pid, handle, base_addr, details)) =
            find_process::find_process(known.clone(), config.force_version.clone())
        {
            connect(&config, pid, handle, base_addr, details, &mut offsets, &mut known, &term);
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

fn create_handlers(
    config: &Config,
    pid: Pid,
    handle: process_memory::ProcessHandle,
    base_addr: usize,
    details: &ProcessDetails,
) -> Vec<Box<dyn Handler>> {
    layout::set_for(&details);
    let (offsets, arch) = (&details.address_offsets, &details.arch);

    // Each is None if the version doesn't have the offsets it needs
    let cutscenes: Option<Box<dyn Handler>> = match &config.record_cutscene_timing {
        CutsceneTiming::On { timing_file, livesplit_port } => {
            CutsceneTimingGeneratorHandler::new(offsets, arch, &base_addr, &handle, timing_file, livesplit_port)
                .map(|h| Box::new(h) as Box<dyn Handler>)
        }
        CutsceneTiming::Off {} => CutsceneHandler::new(
            offsets,
            arch,
            &base_addr,
            &handle,
            &pid,
            &config.cutscene_blacklist_file,
            &config.cutscene_timing_file,
        )
        .map(|h| Box::new(h) as Box<dyn Handler>),
    };
    let overlay = OverlayHandler::new_overlay_handler(offsets, arch, &base_addr, &handle, &pid, &config.overlay, &config.hotkeys);
    let position = PositionHandler::new_position_handler(offsets, arch, &base_addr, &handle, &pid);
    let look_at = PositionHandler::new_look_at_handler(offsets, arch, &base_addr, &handle, &pid);

    let mut handlers: Vec<Box<dyn Handler>> = cutscenes.into_iter().collect();
    handlers.extend(overlay.map(|h| Box::new(h) as Box<dyn Handler>));
    handlers.extend(position.into_iter().chain(look_at).map(|h| Box::new(h) as Box<dyn Handler>));
    handlers
}

fn connect(
    config: &Config,
    pid: Pid,
    handle: process_memory::ProcessHandle,
    base_addr: usize,
    mut details: ProcessDetails,
    offsets: &mut OffsetsSource,
    known: &mut Vec<ProcessDetails>,
    term: &Arc<AtomicBool>,
) {
    println!("Connecting to {} {} with PID {}", details.name, details.version.version, pid);

    let mut handlers = create_handlers(config, pid, handle, base_addr, &details);

    let (tx, rx) = mpsc::channel();

    let key_groups = config
        .hotkeys
        .clone()
        .into_iter()
        .map(|h: Hotkey| -> (KeyCode, Action) { (h.key, h.action) })
        .into_group_map();

    let mut callbacks: std::collections::HashMap<KeyCode, Callback> = std::collections::HashMap::new();
    for (key, actions) in key_groups {
        let current_tx = tx.clone();
        callbacks.insert(key, Box::new(move || {
            for action in &actions {
                let _ = current_tx.send(*action);
            }
        }));
    }
    // Polls the keys until it's dropped, when this returns
    let _hotkeys = Hotkeys::start(callbacks);

    println!("Started!");

    let mut last_offsets_check = time::Instant::now();
    loop {
        if term.load(Ordering::Relaxed) {
            return;
        }

        if !find_process::is_process_running(pid) {
            println!("Disconnected from {} {} with PID {}", details.name, details.version.version, pid);
            return;
        }

        if last_offsets_check.elapsed() > OFFSETS_CHECK_INTERVAL {
            last_offsets_check = time::Instant::now();
            if offsets.changed() {
                *known = load_offsets(offsets, known.clone());
                match find_process::find_process(known.clone(), config.force_version.clone()) {
                    Some((new_pid, new_handle, _, new_details)) if new_pid == pid => {
                        find_process::close_handle(new_handle);
                        // Recreating handlers loses their state (and re-downloads cutscene info), so
                        // only do it when something they use has changed
                        if new_details.version.version != details.version.version
                            || new_details.address_offsets != details.address_offsets
                        {
                            // Drop the old handlers first, so e.g. the overlay is ejected before it's re-injected
                            handlers.clear();
                            details = new_details;
                            handlers = create_handlers(config, pid, handle, base_addr, &details);
                            println!("Reloaded offsets for {} {}", details.name, details.version.version);
                        }
                    }
                    _ => {
                        println!(
                            "{} {} no longer matches any offsets, disconnecting",
                            details.name, details.version.version
                        );
                        return;
                    }
                }
            }
        }

        if let Ok(action) = rx.try_recv() {
            dispatch(&mut handlers, action, config.debug_logging);
        }

        for handler in &mut handlers {
            if let Err(e) = handler.handle_tick() {
                report(&*e, config.debug_logging);
            }
        }

        // The overlay shows the other handlers' state, and its buttons ask for actions
        let mut state = HelperState::default();
        for handler in &handlers {
            handler.report(&mut state);
        }
        let mut requested = vec![];
        for handler in &mut handlers {
            handler.show(&state);
            requested.extend(handler.take_actions());
        }
        for action in requested {
            dispatch(&mut handlers, action, config.debug_logging);
        }
    }
}

fn dispatch(handlers: &mut Vec<Box<dyn Handler>>, action: Action, debug_logging: bool) {
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

fn print_help(hotkeys: &Vec<Hotkey>) {
    for hotkey in hotkeys {
        println!("{:?} => {:?}", hotkey.key, hotkey.action);
    }
}
