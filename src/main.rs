use crate::action::Action;
use crate::config::{Hotkey, CutsceneTiming, Config};
use crate::handler::Handler;
use crate::cutscene_handler::CutsceneHandler;
use crate::cutscene_timing_generator_handler::CutsceneTimingGeneratorHandler;
use crate::overlay_handler::OverlayHandler;
use crate::offsets_file::OffsetsSource;
use crate::position_handler::PositionHandler;
use crate::process_details::ProcessDetails;
use process_memory::Pid;
use itertools::Itertools;
use livesplit_hotkey::Hook;
use livesplit_hotkey::KeyCode;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{thread, time};
use signal_hook::consts::SIGINT;
use signal_hook::flag;

mod action;
mod config;
mod cutscene_handler;
mod cutscene_timing_generator_handler;
mod chains;
mod code_trace;
mod derive;
mod pe_image;
mod position_handler;
mod handler;
mod overlay_handler;
mod position;
mod process_details;
mod find_process;
mod offsets_file;
mod tracked_memory;
mod watch;
mod witness;
mod tracked_position;
mod cutscene_timing_info;
mod readable_from_path;
mod ui_ipc_data;

#[cfg(windows)]
extern crate winapi;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        if let Err(e) = derive::run(&args) {
            eprintln!("{}", e);
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

        thread::sleep(time::Duration::from_millis(100));
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
            eprintln!("Error: {}", e);
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
    let mut handlers: Vec<Box<dyn Handler>> = vec![];

    match &config.record_cutscene_timing {
        CutsceneTiming::On { timing_file, livesplit_port } => {
            match CutsceneTimingGeneratorHandler::new(
                &details.address_offsets,
                &details.arch,
                &base_addr,
                &handle,
                timing_file,
                livesplit_port,
            ) {
                Some(h) => handlers.push(Box::new(h)),
                None => {},
            }
        },
        CutsceneTiming::Off {} => {
            match CutsceneHandler::new(
                &details.address_offsets,
                &details.arch,
                &base_addr,
                &handle,
                &config.cutscene_blacklist_file,
                &config.cutscene_timing_file,
            ) {
                Some(h) => handlers.push(Box::new(h)),
                None => {},
            }
        },
    }

    match OverlayHandler::new_overlay_handler(
        &details.address_offsets,
        &details.arch,
        &base_addr,
        &handle,
        &pid
    ) {
        Some(h) => handlers.push(Box::new(h)),
        None => {},
    }

    match PositionHandler::new_position_handler(
        &details.address_offsets,
        &details.arch,
        &base_addr,
        &handle,
    ) {
        Some(h) => handlers.push(Box::new(h)),
        None => {},
    }

    match PositionHandler::new_look_at_handler(
        &details.address_offsets,
        &details.arch,
        &base_addr,
        &handle,
    ) {
        Some(h) => handlers.push(Box::new(h)),
        None => {},
    }

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

    let hook = Hook::new().unwrap();
    let key_groups = config
        .hotkeys
        .clone()
        .into_iter()
        .map(|h: Hotkey| -> (KeyCode, Action) { (h.key, h.action) })
        .into_group_map();

    for (key, actions) in key_groups {
        let current_tx = tx.clone();
        hook.register(key, move || {
            for action in &actions {
                current_tx.send(*action).unwrap();
            }
        })
        .unwrap();
    }

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

        if last_offsets_check.elapsed() > time::Duration::from_millis(500) {
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

        let signal = rx.try_recv();

        match signal {
            Ok(s) => {
                for handler in &mut handlers {
                    handler
                        .handle_action(s)
                        .unwrap_or_else(|msg| eprintln!("Error: {}", msg));
                }
            }
            _ => {}
        }

        for handler in &mut handlers {
            handler
                .handle_tick()
                .unwrap_or_else(|msg| eprintln!("Error: {}", msg));
        }
    }
}

fn print_help(hotkeys: &Vec<Hotkey>) {
    for hotkey in hotkeys {
        println!("{:?} => {:?}", hotkey.key, hotkey.action);
    }
}
