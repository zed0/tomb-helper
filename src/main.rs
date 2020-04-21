use livesplit_hotkey::{Hook, KeyCode};
use std::thread;
use std::sync::mpsc;
use std::ptr::{null, null_mut};
use process_memory::{Memory, Pid, DataMember, TryIntoProcessHandle};

#[cfg(windows)]
extern crate winapi;

fn main() {
    println!("Attaching to ROTTR.exe...");
    let (tx, rx) = mpsc::channel();

    let memory_thread = thread::spawn(move || {
        let pid = get_pid("ROTTR.exe");
        let handle = pid.try_into_process_handle().unwrap();
        println!("Found ROTTR.exe PID: {}", pid);
        let base_addr = get_base_address(pid) as *const _ as usize;
        println!("Found base memory address: 0x{:X}", base_addr);

        let current_z = DataMember::<f32>::new_offset(handle, vec![base_addr + 0x01_08_2A_B8, 0x18]);
	let mut current_z = current_z.read().unwrap();
	loop {
	    let direction = rx.try_recv();
	    match direction {
	        Ok("up") => {
		    current_z += 100.0;
                }
	        Ok("down") => {
		    current_z -= 100.0;
                }
                _ => {
		}
	    }
            let z = DataMember::<f32>::new_offset(handle, vec![base_addr + 0x01_08_2A_B8, 0x18]);
            z.write(&current_z);
	}
    });

    let hook = Hook::new().unwrap();

    let up_tx = tx.clone();
    hook.register(KeyCode::Space, move || {
        up_tx.send("up").unwrap();
    }).unwrap();

    let down_tx = tx.clone();
    hook.register(KeyCode::C, move || {
        down_tx.send("down").unwrap();
    }).unwrap();

    println!("Started!");
    thread::park();
    hook.unregister(KeyCode::Space).unwrap();
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

