use crate::action::Action;
use crate::config::{Hotkey, CutsceneTiming};
use crate::handler::Handler;
use crate::cutscene_handler::CutsceneHandler;
use crate::cutscene_timing_generator_handler::CutsceneTimingGeneratorHandler;
use crate::position_handler::PositionHandler;
use itertools::Itertools;
use livesplit_hotkey::Hook;
use livesplit_hotkey::KeyCode;
use process_memory::{Pid, TryIntoProcessHandle};
use std::ptr::{null, null_mut};
use std::sync::mpsc;

mod action;
mod config;
mod cutscene_handler;
mod cutscene_timing_generator_handler;
mod position_handler;
mod handler;
mod process_details;
mod tracked_memory;
mod cutscene_timing_info;
mod readable_from_path;

#[cfg(windows)]
extern crate winapi;

fn main() {
    let config = config::get_config();

    let (pid, details) = process_details::known_process_details()
        .iter()
        .find_map(|details| {
            let result = get_pid(&details.executable_name);
            result.map(|r| (r, details.clone()))
        })
        .expect("Could not find any known Tomb Raider process.");

    println!("Found {} with PID {}", details.name, pid);

    let handle = pid.try_into_process_handle().unwrap();
    let base_addr = get_base_address(pid) as *const _ as usize;
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
    print_help(&config.hotkeys);

    loop {
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

#[cfg(windows)]
pub fn get_base_address(pid: Pid) -> *const u8 {
    let mut module = winapi::um::tlhelp32::MODULEENTRY32 {
        dwSize: std::mem::size_of::<winapi::um::tlhelp32::MODULEENTRY32>() as u32,
        th32ModuleID: 0,
        th32ProcessID: 0,
        GlblcntUsage: 0,
        ProccntUsage: 0,
        modBaseAddr: null_mut(),
        modBaseSize: 0,
        hModule: null_mut(),
        szModule: [0; 256],
        szExePath: [0; 260],
    };

    let snapshot: process_memory::ProcessHandle;
    unsafe {
        snapshot = winapi::um::tlhelp32::CreateToolhelp32Snapshot(
            //winapi::um::tlhelp32::TH32CS_SNAPALL,
            winapi::um::tlhelp32::TH32CS_SNAPMODULE | winapi::um::tlhelp32::TH32CS_SNAPMODULE32,
            pid,
        );
        if snapshot == winapi::um::handleapi::INVALID_HANDLE_VALUE {
            return null();
        }

        if winapi::um::tlhelp32::Module32First(snapshot, &mut module)
            == winapi::shared::minwindef::TRUE
        {
            return module.modBaseAddr;
        }
    }
    null()
}

#[cfg(windows)]
pub fn get_pid(process_name: &str) -> Option<Pid> {
    /// A helper function to turn a c_char array to a String
    fn utf8_to_string(bytes: &[i8]) -> String {
        use std::ffi::CStr;
        unsafe {
            CStr::from_ptr(bytes.as_ptr())
                .to_string_lossy()
                .into_owned()
        }
    }
    let mut entry = winapi::um::tlhelp32::PROCESSENTRY32 {
        dwSize: std::mem::size_of::<winapi::um::tlhelp32::PROCESSENTRY32>() as u32,
        cntUsage: 0,
        th32ProcessID: 0,
        th32DefaultHeapID: 0,
        th32ModuleID: 0,
        cntThreads: 0,
        th32ParentProcessID: 0,
        pcPriClassBase: 0,
        dwFlags: 0,
        szExeFile: [0; winapi::shared::minwindef::MAX_PATH],
    };
    let snapshot: process_memory::ProcessHandle;
    unsafe {
        snapshot = winapi::um::tlhelp32::CreateToolhelp32Snapshot(
            winapi::um::tlhelp32::TH32CS_SNAPPROCESS,
            0,
        );
        if winapi::um::tlhelp32::Process32First(snapshot, &mut entry)
            == winapi::shared::minwindef::TRUE
        {
            while winapi::um::tlhelp32::Process32Next(snapshot, &mut entry)
                == winapi::shared::minwindef::TRUE
            {
                if utf8_to_string(&entry.szExeFile) == process_name {
                    winapi::um::handleapi::CloseHandle(snapshot);
                    return Some(entry.th32ProcessID);
                }
            }
        }
        winapi::um::handleapi::CloseHandle(snapshot);
    }
    None
}
