//! Running one of the game's functions in a thread of the game's own: a stub that puts four arguments where the
//! function takes them and calls it, started with `CreateRemoteThread` and waited for. In a 64-bit game that's the
//! Windows x64 calling convention (rcx, rdx, r8, r9); in a 32-bit one it's `__thiscall` (the first in ecx, the rest on
//! the stack, which the function pops), as TR2013's send-message function is. The game's main thread keeps running meanwhile, so only call what the game
//! itself calls from other threads (behind a lock). Used to send messages (`skew`).
//!
//! Its page (the stub, and a zeroed buffer for arguments to point at, `data`) is left in the game when this is dropped,
//! as what's called may keep a pointer into the buffer.

use process_memory::{Architecture, Pid};
use std::io;
use std::ptr::null_mut;
use winapi::shared::minwindef::{DWORD, FALSE};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::handleapi::CloseHandle;
use winapi::um::memoryapi::{VirtualAllocEx, WriteProcessMemory};
use winapi::um::processthreadsapi::{CreateRemoteThread, OpenProcess};
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winnt::{
    HANDLE, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION,
    PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE, SYNCHRONIZE,
};

const PAGE_SIZE: usize = 0x1000;
// Where the arguments' buffer starts in the page
const DATA: usize = 0x800;
// How long a call can take before it's given up on (it's left running)
const TIMEOUT_MS: DWORD = 5000;

pub struct GameCall {
    process: HANDLE,
    page: usize,
    arch: Architecture,
}

impl GameCall {
    /// Opens the game to call into it, and makes the page in it
    pub fn new(pid: Pid, arch: Architecture) -> io::Result<GameCall> {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_READ
            | PROCESS_VM_WRITE
            | SYNCHRONIZE;
        let process = unsafe { OpenProcess(access, FALSE, pid) };
        if process.is_null() {
            return Err(last_error("Could not open the game to call into it"));
        }
        let page = unsafe { VirtualAllocEx(process, null_mut(), PAGE_SIZE, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) };
        if page.is_null() {
            let error = last_error("Could not make a page in the game to call from");
            unsafe { CloseHandle(process) };
            return Err(error);
        }
        if matches!(arch, Architecture::Arch32Bit) && page as usize > u32::MAX as usize {
            unsafe { CloseHandle(process) };
            return Err(io::Error::other("The page made in the 32-bit game is out of its reach"));
        }
        Ok(GameCall { process, page: page as usize, arch })
    }

    /// A zeroed buffer in the game (0x800 bytes), for an argument to point at
    pub fn data(&self) -> usize {
        self.page + DATA
    }

    /// Calls `function` (an address in the game) with these arguments, in a thread of the game's, and waits for it
    pub fn call(&self, function: usize, args: [u64; 4]) -> io::Result<()> {
        let stub = match self.arch {
            Architecture::Arch32Bit => stub32(function, args)?,
            _ => stub(function as u64, args),
        };
        let mut written = 0;
        let ok = unsafe {
            WriteProcessMemory(self.process, self.page as *mut _, stub.as_ptr() as *const _, stub.len(), &mut written)
        };
        if ok == FALSE || written != stub.len() {
            return Err(last_error("Could not write the call into the game"));
        }
        let start: unsafe extern "system" fn(*mut winapi::ctypes::c_void) -> DWORD = unsafe { std::mem::transmute(self.page) };
        let thread = unsafe { CreateRemoteThread(self.process, null_mut(), 0, Some(start), null_mut(), 0, null_mut()) };
        if thread.is_null() {
            return Err(last_error("Could not start the call in the game"));
        }
        let waited = unsafe { WaitForSingleObject(thread, TIMEOUT_MS) };
        unsafe { CloseHandle(thread) };
        if waited == WAIT_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "The call in the game didn't finish"));
        }
        Ok(())
    }
}

impl Drop for GameCall {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.process) };
    }
}

// The code the thread runs: the arguments into rcx, rdx, r8 and r9, call, return 0
fn stub(function: u64, args: [u64; 4]) -> Vec<u8> {
    let mut code = vec![0x48, 0x83, 0xEC, 0x28]; // sub rsp, 0x28 (shadow space, and keeps the stack aligned)
    for (prefix, value) in [[0x48, 0xB9], [0x48, 0xBA], [0x49, 0xB8], [0x49, 0xB9]].iter().zip(args) {
        code.extend_from_slice(prefix); // mov rcx/rdx/r8/r9, value
        code.extend_from_slice(&value.to_le_bytes());
    }
    code.extend_from_slice(&[0x48, 0xB8]); // mov rax, function
    code.extend_from_slice(&function.to_le_bytes());
    code.extend_from_slice(&[0xFF, 0xD0]); // call rax
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.extend_from_slice(&[0x31, 0xC0, 0xC3]); // xor eax, eax; ret
    code
}

// The same for a 32-bit `__thiscall` function: the last three arguments pushed, the first in ecx, call, return 0 (popping
// the thread's parameter)
fn stub32(function: usize, args: [u64; 4]) -> io::Result<Vec<u8>> {
    let narrow = |value: u64| {
        u32::try_from(value).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "A 32-bit call's argument is too big"))
    };
    let mut code = vec![];
    for &value in args[1..].iter().rev() {
        code.push(0x68); // push value
        code.extend_from_slice(&narrow(value)?.to_le_bytes());
    }
    code.push(0xB9); // mov ecx, value
    code.extend_from_slice(&narrow(args[0])?.to_le_bytes());
    code.push(0xB8); // mov eax, function
    code.extend_from_slice(&narrow(function as u64)?.to_le_bytes());
    code.extend_from_slice(&[0xFF, 0xD0]); // call eax
    code.extend_from_slice(&[0x31, 0xC0, 0xC2, 0x04, 0x00]); // xor eax, eax; ret 4
    Ok(code)
}

fn last_error(what: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{}: {}", what, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The stub as it was run by hand in SOTTR 492 to turn skew on
    #[test]
    fn the_stub_loads_the_arguments_and_calls() {
        let code = stub(0x1122, [1, 2, 3, 4]);
        assert_eq!(&code[..6], &[0x48, 0x83, 0xEC, 0x28, 0x48, 0xB9]);
        assert_eq!(&code[6..14], &1u64.to_le_bytes());
        assert_eq!(&code[14..16], &[0x48, 0xBA]);
        assert_eq!(&code[24..26], &[0x49, 0xB8]);
        assert_eq!(&code[34..36], &[0x49, 0xB9]);
        assert_eq!(&code[44..46], &[0x48, 0xB8]);
        assert_eq!(&code[46..54], &0x1122u64.to_le_bytes());
        assert_eq!(&code[54..], &[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x28, 0x31, 0xC0, 0xC3]);
    }

    #[test]
    fn the_32_bit_stub_pushes_the_arguments_and_puts_the_first_in_ecx() {
        let code = stub32(0x1122, [1, 2, 3, 4]).unwrap();
        let mut expected = vec![0x68, 4, 0, 0, 0, 0x68, 3, 0, 0, 0, 0x68, 2, 0, 0, 0, 0xB9, 1, 0, 0, 0];
        expected.extend_from_slice(&[0xB8, 0x22, 0x11, 0, 0, 0xFF, 0xD0, 0x31, 0xC0, 0xC2, 0x04, 0x00]);
        assert_eq!(code, expected);
        assert!(stub32(0x1122, [1 << 32, 2, 3, 4]).is_err());
    }
}
