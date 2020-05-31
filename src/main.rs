use livesplit_hotkey::Hook;
use process_memory::{DataMember, Memory, Pid, ProcessHandle, TryIntoProcessHandle, Architecture};
use std::fmt;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use crate::action::Action;
use crate::config::Hotkey;
use crate::process_details::AddressType;

mod process_details;
mod action;
mod config;

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

    let mut active = false;
    let mut position = TrackedPosition::new(
        details.address_offsets[&AddressType::XPosition].clone(),
        details.address_offsets[&AddressType::YPosition].clone(),
        details.address_offsets[&AddressType::ZPosition].clone(),
        details.arch,
        base_addr,
    );

    let mut look_at = {
        if details.address_offsets.contains_key(&AddressType::XLookAt)
            && details.address_offsets.contains_key(&AddressType::YLookAt)
            && details.address_offsets.contains_key(&AddressType::ZLookAt)
        {
            Some(TrackedPosition::new(
                details.address_offsets[&AddressType::XLookAt].clone(),
                details.address_offsets[&AddressType::YLookAt].clone(),
                details.address_offsets[&AddressType::ZLookAt].clone(),
                details.arch,
                base_addr,
            ))
        } else {
            None
        }

    };

    let mut cutscene_status = {
        if details.address_offsets.contains_key(&AddressType::CutsceneStatus) {
            Some(TrackedMemory::<u8> {
                data: 0,
                offsets: details.address_offsets[&AddressType::CutsceneStatus].clone(),
                arch: details.arch,
                base_addr: base_addr,
            })
        } else {
            None
        }
    };

    let mut saved_position = position.clone();
    let mut saved_look_at = look_at.clone();

    let (tx, rx) = mpsc::channel();

    let hook = Hook::new().unwrap();
    for hotkey in config.hotkeys.clone() {
        let current_tx = tx.clone();
        hook.register(hotkey.key, move || {
            current_tx.send(hotkey.action).unwrap();
        })
        .unwrap();
    }

    println!("Started!");
    print_help(&config.hotkeys);

    loop {
        let signal = rx.try_recv();
        match signal {
            Ok(Action::ToggleActive{}) => {
                if active {
                    active = false;
                    println!("Deactivated");
                } else {
                    active = true;
                    match position.fetch_from_game(handle) {
                        Err(msg) => eprintln!("Error activating: {}", msg),
                        Ok(()) => println!("Activated"),
                    }
                }
            }
            Ok(Action::StorePosition{}) => {
                saved_position = position.clone();
                match saved_position.fetch_from_game(handle) {
                    Err(msg) => {
                        eprintln!("Error storing position: {}", msg);
                        return;
                    },
                    Ok(()) => {},
                }
                saved_look_at = look_at.clone();
                match &mut saved_look_at {
                    Some(s) => {
                        match s.fetch_from_game(handle) {
                            Err(msg) => {
                                eprintln!("Error storing look_at: {}", msg);
                                return;
                            },
                            Ok(()) => {},
                        }

                    }
                    None => {}
                }

                println!("Stored! (position: {:} look_at: {:})", saved_position, option_fmt(&saved_look_at));
            }
            Ok(Action::RestorePosition{}) => {
                position = saved_position.clone();
                match position.apply_to_game(handle) {
                    Err(msg) => {
                        eprintln!("Error restoring position: {}", msg);
                        return;
                    },
                    Ok(()) => {},
                }
                look_at = saved_look_at.clone();
                match &mut saved_look_at {
                    Some(s) => {
                        match s.apply_to_game(handle) {
                            Err(msg) => {
                                eprintln!("Error restoring look_at: {}", msg);
                                return;
                            },
                            Ok(()) => {},
                        }
                    },
                    None => {},
                }
                println!("Restored! (position: {:}, look_at: {:})", position, option_fmt(&look_at));
            }
            Ok(Action::SkipCutscene{}) => {
                match &mut cutscene_status {
                    Some(s) => {
                        s.data = 5;
                        match s.apply_to_game(handle) {
                            Err(msg) => println!("Could not skip cutscene: {}", msg),
                            Ok(()) => println!("Skipped cutscene"),
                        }
                    },
                    None => {},
                }
            }
            Ok(Action::Forward{distance}) => {
                if active {
                    position.x.data += distance;
                }
            }
            Ok(Action::Backward{distance}) => {
                if active {
                    position.x.data -= distance;
                }
            }
            Ok(Action::Left{distance}) => {
                if active {
                    position.y.data += distance;
                }
            }
            Ok(Action::Right{distance}) => {
                if active {
                    position.y.data -= distance;
                }
            }
            Ok(Action::Up{distance}) => {
                if active {
                    position.z.data += distance;
                }
            }
            Ok(Action::Down{distance}) => {
                if active {
                    position.z.data -= distance;
                }
            }
            _ => {}
        }

        if active {
            match position.apply_to_game(handle) {
                Err(msg) => eprintln!("Error updating position: {}", msg),
                Ok(()) => {},
            }
        }
    }
}

fn option_fmt<T: fmt::Display>(opt: & Option<T>) -> String {
    opt.as_ref().map_or(String::from("None"), |p| format!("{}", p))
}

fn print_help(hotkeys: &Vec<Hotkey>) {
    for hotkey in hotkeys {
        println!("{:?} => {:?}", hotkey.key, hotkey.action);
    }
}

#[derive(Debug, Clone)]
struct TrackedPosition {
    x: TrackedMemory<f32>,
    y: TrackedMemory<f32>,
    z: TrackedMemory<f32>,
}

impl TrackedPosition {
    fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.fetch_from_game(handle)?;
        self.y.fetch_from_game(handle)?;
        self.z.fetch_from_game(handle)?;
        Ok(())
    }

    fn apply_to_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.apply_to_game(handle)?;
        self.y.apply_to_game(handle)?;
        self.z.apply_to_game(handle)?;
        Ok(())
    }

    fn new(
        x_offsets: Vec<usize>,
        y_offsets: Vec<usize>,
        z_offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedPosition {
        TrackedPosition {
            x: TrackedMemory {
                data: 0.0,
                offsets: x_offsets,
                arch: arch,
                base_addr: base_addr,
            },
            y: TrackedMemory {
                data: 0.0,
                offsets: y_offsets,
                arch: arch,
                base_addr: base_addr,
            },
            z: TrackedMemory {
                data: 0.0,
                offsets: z_offsets,
                arch: arch,
                base_addr: base_addr,
            },
        }
    }
}

impl fmt::Display for TrackedPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x.data, self.y.data, self.z.data)
    }
}

#[derive(Debug, Clone)]
struct TrackedMemory<T: Copy> {
    data: T,
    offsets: Vec<usize>,
    arch: Architecture,
    base_addr: usize,
}

impl<T: Copy + std::fmt::Debug> TrackedMemory<T> {
    fn offsets_with_base(&self) -> Vec<usize> {
        let mut offsets_with_base = self.offsets.clone();
        offsets_with_base[0] += self.base_addr;
        offsets_with_base
    }

    fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.data = DataMember::<T>::new_offset(handle, self.offsets_with_base())
            .set_arch(self.arch)
            .read()?;
        Ok(())
    }

    fn apply_to_game(&self, handle: ProcessHandle) -> io::Result<()> {
        DataMember::<T>::new_offset(handle, self.offsets_with_base())
            .set_arch(self.arch)
            .write(&self.data)?;
        Ok(())
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
        if snapshot == winapi::um::handleapi::INVALID_HANDLE_VALUE
        {
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
