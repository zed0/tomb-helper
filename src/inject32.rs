//! Injecting the overlay into a 32-bit game (TR2013). hudhook's injector starts `LoadLibraryW` in the game at the
//! address it has in this process, which is the 64-bit one; a 32-bit game has its own 32-bit `kernel32.dll`, so the
//! address is found from that module's export table, read out of the game.

use process_memory::CopyAddress;
use process_memory::Pid;
use std::convert::TryInto;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use winapi::shared::minwindef::{DWORD, FALSE, HMODULE, MAX_PATH};
use winapi::um::handleapi::CloseHandle;
use winapi::um::memoryapi::{VirtualAllocEx, VirtualFreeEx, WriteProcessMemory};
use winapi::um::processthreadsapi::{CreateRemoteThread, GetExitCodeThread, OpenProcess};
use winapi::um::psapi::{EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_32BIT};
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::INFINITE;
use winapi::um::winnt::{
    HANDLE, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION,
    PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

// Modules listed at most
const MAX_MODULES: usize = 1024;

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn last_error(what: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{}: {}", what, error))
}

/// Where the 32-bit module with this name (ignoring case) is loaded in the process
fn module_base(process: HANDLE, name: &str) -> io::Result<usize> {
    let mut modules: Vec<HMODULE> = vec![null_mut(); MAX_MODULES];
    let mut needed: DWORD = 0;
    let listed = unsafe {
        EnumProcessModulesEx(
            process,
            modules.as_mut_ptr(),
            (modules.len() * std::mem::size_of::<HMODULE>()) as DWORD,
            &mut needed,
            LIST_MODULES_32BIT,
        )
    };
    if listed == FALSE {
        return Err(last_error("Could not list the game's modules"));
    }
    modules.truncate(needed as usize / std::mem::size_of::<HMODULE>());
    modules
        .into_iter()
        .find(|module| {
            let mut buffer = [0_u16; MAX_PATH];
            let length = unsafe { GetModuleBaseNameW(process, *module, buffer.as_mut_ptr(), MAX_PATH as DWORD) };
            String::from_utf16_lossy(&buffer[..length as usize]).eq_ignore_ascii_case(name)
        })
        .map(|module| module as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("The game has no 32-bit {}", name)))
}

/// The address of an exported function of a module loaded in the process
fn export_address(process: HANDLE, module: usize, function: &str) -> io::Result<usize> {
    let read = |address: usize, length: usize| -> io::Result<Vec<u8>> {
        let mut bytes = vec![0_u8; length];
        process.copy_address(address, &mut bytes)?;
        Ok(bytes)
    };
    let u32_at = |address: usize| -> io::Result<usize> {
        Ok(u32::from_le_bytes(read(address, 4)?.try_into().unwrap()) as usize)
    };
    let headers = module + u32_at(module + 0x3C)?;
    // The export directory is the first data directory of a PE32 optional header
    let exports = module + u32_at(headers + 24 + 96)?;
    let name_count = u32_at(exports + 24)?;
    let functions = module + u32_at(exports + 28)?;
    let names = module + u32_at(exports + 32)?;
    let ordinals = module + u32_at(exports + 36)?;
    for i in 0..name_count {
        let name = read(module + u32_at(names + i * 4)?, function.len() + 1)?;
        if &name[..function.len()] == function.as_bytes() && name[function.len()] == 0 {
            let ordinal = u16::from_le_bytes(read(ordinals + i * 2, 2)?.try_into().unwrap()) as usize;
            return Ok(module + u32_at(functions + ordinal * 4)?);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{} isn't exported", function)))
}

/// Loads the (32-bit) DLL into the 32-bit game, by running its `LoadLibraryW` on the path
pub fn inject(pid: Pid, dll: &Path) -> io::Result<()> {
    let access =
        PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE;
    let process = unsafe { OpenProcess(access, FALSE, pid) };
    if process.is_null() {
        return Err(last_error("Could not open the game"));
    }
    let process = OwnedHandle(process);
    let load_library = export_address(process.0, module_base(process.0, "kernel32.dll")?, "LoadLibraryW")?;

    let path: Vec<u16> = OsStr::new(dll).encode_wide().chain(Some(0)).collect();
    let size = path.len() * 2;
    let remote = unsafe { VirtualAllocEx(process.0, null_mut(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
    if remote.is_null() {
        return Err(last_error("Could not allocate memory in the game"));
    }
    let result = (|| {
        if unsafe { WriteProcessMemory(process.0, remote, path.as_ptr() as _, size, null_mut()) } == FALSE {
            return Err(last_error("Could not write the DLL's path into the game"));
        }
        let start = unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(_) -> DWORD>(load_library) };
        let thread = unsafe { CreateRemoteThread(process.0, null_mut(), 0, Some(start), remote, 0, null_mut()) };
        if thread.is_null() {
            return Err(last_error("Could not start LoadLibraryW in the game"));
        }
        let thread = OwnedHandle(thread);
        unsafe { WaitForSingleObject(thread.0, INFINITE) };
        // The thread's exit code is what LoadLibraryW returned: the module, or 0 if it failed
        let mut loaded: DWORD = 0;
        unsafe { GetExitCodeThread(thread.0, &mut loaded) };
        if loaded == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, format!("The game couldn't load {:?}", dll)));
        }
        Ok(())
    })();
    unsafe { VirtualFreeEx(process.0, remote, 0, MEM_RELEASE) };
    result
}
