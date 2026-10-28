//! Finding the game: process and module lookup (`get_pid`, `get_base_address`, `is_process_running`), and which known
//! version it is, by image size first, else by the version string in its memory (or `force_version`).

use crate::game::Game;
use crate::game_memory::{self, Handle};
use crate::process_details::{AddressType, ProcessDetails};
use process_memory::{CopyAddress, DataMember, Memory, Pid, ProcessHandle, TryIntoProcessHandle};
use std::ptr::{null, null_mut};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::winnt::SYNCHRONIZE;

/// A running game, of a version in the offsets file
pub struct Connection {
    pub pid: Pid,
    pub process: ProcessHandle,
    pub base_address: usize,
    pub details: ProcessDetails,
}

impl Connection {
    /// For reading its memory
    pub fn handle(&self) -> Handle {
        Handle::new(game_memory::Memory::Live(self.pid, self.process), &self.details)
    }

    /// For finding things in it, through its version's offsets
    pub fn game(&self) -> Game {
        Game::new(self.handle(), self.base_address, &self.details.address_offsets)
    }
}

/// The running game, if it's one of these versions (or `force_version`)
pub fn find_process(possible_processes: Vec<ProcessDetails>, force_version: Option<String>) -> Option<Connection> {
    possible_processes.iter().find_map(|details| {
        let pid = get_pid(&details.executable_name)?;
        let handle = pid.try_into_process_handle().ok()?;
        let base_addr = get_base_address(pid) as *const _ as usize;
        let connection = || Some(Connection { pid, process: handle, base_address: base_addr, details: details.clone() });

        if force_version == Some(details.version.version.clone()) {
            println!(
                "Warning: Forcing version to {}, some functions may not work as expected!",
                details.version.version
            );
            return connection();
        }

        // Try using the image size first, then the version string
        if let Some(image_size) = details.version.image_size {
            if get_image_size(handle, base_addr).ok()? != image_size {
                return None;
            }
            return connection();
        }

        let version_in_memory = try_read_std_string_utf8(
            handle,
            details.address_offsets.get(&AddressType::Version)?.clone(),
            base_addr,
            details.version.version.len(),
            details.arch,
        )
        .ok()?;

        if version_in_memory != details.version.version {
            None
        } else {
            connection()
        }
    })
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

#[cfg(windows)]
pub fn is_process_running(pid: Pid) -> bool {
    unsafe {
        let process = winapi::um::processthreadsapi::OpenProcess(SYNCHRONIZE, 0, pid);
        let result = winapi::um::synchapi::WaitForSingleObject(process, 0);
        winapi::um::handleapi::CloseHandle(process);
        result == WAIT_TIMEOUT
    }
}

#[cfg(windows)]
pub fn close_handle(handle: process_memory::ProcessHandle) {
    unsafe {
        winapi::um::handleapi::CloseHandle(handle);
    }
}

#[cfg(not(windows))]
pub fn close_handle(_handle: process_memory::ProcessHandle) {
    panic!("tomb-helper is only supported on Windows");
}

/// The size of the module in memory, from its PE headers
pub fn get_image_size(handle: impl CopyAddress, base_addr: usize) -> Result<usize, std::io::Error> {
    use crate::pe_image::{OPTIONAL_HEADER, PE_SIGNATURE_OFFSET, SIZE_OF_IMAGE};
    let mut pe_signature_offset_bytes = [0_u8; 4];
    handle.copy_address(base_addr + PE_SIGNATURE_OFFSET, &mut pe_signature_offset_bytes)?;
    let pe_signature_offset = u32::from_le_bytes(pe_signature_offset_bytes) as usize;

    let mut image_size_bytes = [0_u8; 4];
    handle.copy_address(base_addr + pe_signature_offset + OPTIONAL_HEADER + SIZE_OF_IMAGE, &mut image_size_bytes)?;
    let image_size = u32::from_le_bytes(image_size_bytes) as usize;

    Ok(image_size)
}

pub fn try_read_std_string_utf8(
    handle: process_memory::ProcessHandle,
    offsets: Vec<usize>,
    base_addr: usize,
    length: usize,
    arch: process_memory::Architecture,
) -> Result<String, std::io::Error> {
    let mut offsets_with_base = offsets.clone();
    offsets_with_base[0] += base_addr;

    let offset = DataMember::<u8>::new_offset(handle, offsets_with_base).set_arch(arch);
    let mut bytes = vec![0_u8; length];
    handle.copy_address(offset.get_offset()?, &mut bytes)?;

    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

#[cfg(not(windows))]
pub fn get_base_address(_pid: Pid) -> *const u8 {
    panic!("tomb-helper is only supported on Windows");
}

#[cfg(not(windows))]
pub fn get_pid(_process_name: &str) -> Option<Pid> {
    panic!("tomb-helper is only supported on Windows");
}

#[cfg(not(windows))]
pub fn is_process_running(_pid: Pid) -> bool {
    panic!("tomb-helper is only supported on Windows");
}
