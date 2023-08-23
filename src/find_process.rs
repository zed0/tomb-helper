use crate::process_details::{AddressType, ProcessDetails};
use process_memory::{CopyAddress, DataMember, Memory, Pid, TryIntoProcessHandle};
use std::ptr::{null, null_mut};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::winnt::SYNCHRONIZE;

pub fn find_process(
    possible_processes: Vec<ProcessDetails>,
    force_version: Option<String>,
) -> Option<(Pid, process_memory::ProcessHandle, usize, ProcessDetails)> {
    possible_processes.iter().find_map(|details| {
        let pid = get_pid(&details.executable_name)?;
        let handle = pid.try_into_process_handle().ok()?;
        let base_addr = get_base_address(pid) as *const _ as usize;

        if force_version == Some(details.version.version.clone()) {
            println!(
                "Warning: Forcing version to {}, some functions may not work as expected!",
                details.version.version
            );
            return Some((pid, handle, base_addr, details.clone()));
        }

        // Try using the image size first, then the version string
        if let Some(image_size) = details.version.image_size {
            let a = get_image_size(handle, base_addr).ok()?;
            if image_size != a {
                return None;
            }

            return Some((pid, handle, base_addr, details.clone()));
        }

        let version_in_memory = try_read_std_string_utf8(
            handle,
            details.address_offsets.get(&AddressType::Version)?.clone(),
            base_addr,
            details.version.version.len(),
        )
        .ok()?;

        if version_in_memory != details.version.version {
            None
        } else {
            Some((pid, handle, base_addr, details.clone()))
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
        return result == WAIT_TIMEOUT;
    }
}

pub fn get_image_size(
    handle: process_memory::ProcessHandle,
    base_addr: usize,
) -> Result<usize, std::io::Error> {
    let mut image_optional_header_offset_bytes = [0_u8; 4];
    handle.copy_address(base_addr + 0x3C, &mut image_optional_header_offset_bytes)?;
    let image_optional_header_offset = u32::from_le_bytes(image_optional_header_offset_bytes) as usize;

    let mut image_size_bytes = [0_u8; 4];
    handle.copy_address(base_addr + image_optional_header_offset + 0x50, &mut image_size_bytes)?;
    let image_size = u32::from_le_bytes(image_size_bytes) as usize;

    Ok(image_size)
}

pub fn try_read_std_string_utf8(
    handle: process_memory::ProcessHandle,
    offsets: Vec<usize>,
    base_addr: usize,
    length: usize,
) -> Result<String, std::io::Error> {
    let mut offsets_with_base = offsets.clone();
    offsets_with_base[0] += base_addr;

    let offset = DataMember::<u8>::new_offset(handle, offsets_with_base);
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
