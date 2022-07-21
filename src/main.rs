use crate::action::Action;
use crate::config::{Hotkey, CutsceneTiming, Config};
use crate::handler::Handler;
use crate::cutscene_handler::CutsceneHandler;
use crate::cutscene_timing_generator_handler::CutsceneTimingGeneratorHandler;
use crate::position_handler::PositionHandler;
use crate::process_details::ProcessDetails;
use process_memory::Pid;
use itertools::Itertools;
use livesplit_hotkey::Hook;
use livesplit_hotkey::KeyCode;
use std::sync::mpsc;
use std::{thread, time};

mod action;
mod config;
mod cutscene_handler;
mod cutscene_timing_generator_handler;
mod position_handler;
mod handler;
mod process_details;
mod find_process;
mod tracked_memory;
mod cutscene_timing_info;
mod readable_from_path;

#[cfg(windows)]
extern crate winapi;

fn main() {
    let config = config::get_config();
    print_help(&config.hotkeys);
    println!("Searching for Tomb Raider processes...");
    loop {
        find_process::find_process(
            process_details::known_process_details(),
            config.force_version.clone(),
        )
        .and_then(|(pid, handle, base_addr, details)| Some(connect(config.clone(), pid, handle, base_addr, details)));

        thread::sleep(time::Duration::from_millis(100));
    }
}

fn connect(
    config: Config,
    pid: Pid,
    handle: process_memory::ProcessHandle,
    base_addr: usize,
    details: ProcessDetails
) {
    println!("Connecting to {} {} with PID {}", details.name, details.version, pid);

    let mut handlers: Vec<Box<dyn Handler>> = vec![];

    match config.record_cutscene_timing {
        CutsceneTiming::On { timing_file, livesplit_port } => {
            match CutsceneTimingGeneratorHandler::new(
                &details.address_offsets,
                &details.arch,
                &base_addr,
                &handle,
                &timing_file,
                &livesplit_port,
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

    loop {
        if !find_process::is_process_running(pid) {
            println!("Disconnected from {} {} with PID {}", details.name, details.version, pid);
            return;
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
