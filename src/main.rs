use livesplit_hotkey::{Hook, KeyCode};
use process_memory::{DataMember, Memory, Pid, ProcessHandle, TryIntoProcessHandle};
use std::ptr::{null, null_mut};
use std::sync::mpsc;

#[cfg(windows)]
extern crate winapi;

fn main() {
    let keys = vec![
        (KeyCode::F5, Action::ToggleActive),
        (KeyCode::W, Action::Forward),
        (KeyCode::S, Action::Backward),
        (KeyCode::A, Action::Left),
        (KeyCode::D, Action::Right),
        (KeyCode::Space, Action::Up),
        (KeyCode::C, Action::Down),
    ];

    println!("Attaching to ROTTR.exe...");
    let (tx, rx) = mpsc::channel();

    let hook = Hook::new().unwrap();
    for (key, action) in keys.clone() {
        let current_tx = tx.clone();
        hook.register(key, move || {
            current_tx.send(action).unwrap();
        })
        .unwrap();
    }

    let pid = get_pid("ROTTR.exe");
    let handle = pid.try_into_process_handle().unwrap();
    println!("Found ROTTR.exe PID: {}", pid);
    let base_addr = get_base_address(pid) as *const _ as usize;
    println!("Found base memory address: 0x{:X}", base_addr);

    let mut active = false;
    let mut position = Position {
        x: TrackedMemory {
            data: 0.0,
            offsets: vec![base_addr + 0x01_08_2A_B8, 0x10],
        },
        y: TrackedMemory {
            data: 0.0,
            offsets: vec![base_addr + 0x01_08_2A_B8, 0x14],
        },
        z: TrackedMemory {
            data: 0.0,
            offsets: vec![base_addr + 0x01_08_2A_B8, 0x18],
        },
    };

    println!("Started!");
    print_help(&keys);

    loop {
        let signal = rx.try_recv();
        if active {
            match signal {
                Ok(Action::ToggleActive) => {
                    active = false;
                    println!("deactivated");
                }
                Ok(Action::Forward) => position.x.data += 100.0,
                Ok(Action::Backward) => position.x.data -= 100.0,
                Ok(Action::Left) => position.y.data += 100.0,
                Ok(Action::Right) => position.y.data -= 100.0,
                Ok(Action::Up) => position.z.data += 100.0,
                Ok(Action::Down) => position.z.data -= 100.0,
                _ => {}
            }

            position.apply_to_game(handle);
        } else {
            match signal {
                Ok(Action::ToggleActive) => {
                    active = true;
                    position.fetch_from_game(handle);
                    println!("activated");
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
enum Action {
    ToggleActive,
    Forward,
    Backward,
    Left,
    Right,
    Up,
    Down,
}

fn print_help(keys: &Vec<(KeyCode, Action)>) {
    for (key, action) in keys {
        println!("{:?} => {:?}", key, action);
    }
}

struct Position {
    x: TrackedMemory<f32>,
    y: TrackedMemory<f32>,
    z: TrackedMemory<f32>,
}

impl Position {
    fn fetch_from_game(&mut self, handle: ProcessHandle) {
        self.x.fetch_from_game(handle);
        self.y.fetch_from_game(handle);
        self.z.fetch_from_game(handle);
    }

    fn apply_to_game(&mut self, handle: ProcessHandle) {
        self.x.apply_to_game(handle);
        self.y.apply_to_game(handle);
        self.z.apply_to_game(handle);
    }
}

struct TrackedMemory<T: Copy> {
    data: T,
    offsets: Vec<usize>,
}

impl<T: Copy> TrackedMemory<T> {
    fn fetch_from_game(&mut self, handle: ProcessHandle) {
        self.data = DataMember::<T>::new_offset(handle, self.offsets.clone())
            .read()
            .unwrap();
    }

    fn apply_to_game(&self, handle: ProcessHandle) {
        DataMember::<T>::new_offset(handle, self.offsets.clone())
            .write(&self.data)
            .unwrap();
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
            winapi::um::tlhelp32::TH32CS_SNAPALL,
            pid,
        );
        if winapi::um::tlhelp32::Module32First(snapshot, &mut module)
            == winapi::shared::minwindef::TRUE
        {
            return module.modBaseAddr;
        }
    }
    null()
}

#[cfg(windows)]
pub fn get_pid(process_name: &str) -> Pid {
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
                    return entry.th32ProcessID;
                }
            }
        }
    }
    0
}
