//! Finds the instructions that access a value ("witnesses") by attaching as a debugger for a few
//! seconds and putting hardware breakpoints on it. This finds code that a static trace can't reach,
//! e.g. fields read through objects found in lists. `derive` then matches these instructions in a
//! new build to find where the field moved to.
//!
//! It sets DR0-3 on up to 4 values (resolved after `--delay`) and records each accessing instruction with its callers
//! (return addresses scanned off the stack); `--write` breaks on writes only. In a 32-bit game the debug registers are
//! in the native context and the registers the code sees in the WOW64 one (`Context`); an access by a `call [mem]` is
//! found at the return address. Hidden `watch-raw <pid> <addr or +rva>[,...]` does the same for any addresses (`--exec`
//! breaks on running the instruction there and records the registers; `--dump <register>:<hex length>` also copies the
//! memory a register points at, the first 3 times per instruction, e.g. `esp:40` for a function's arguments), and
//! `disasm` prints instructions with their RVAs.

use anyhow::{bail, Context as _, Result};
use crate::derive::format_offsets;
use crate::find_process;
use crate::offsets_file::{known_process_details, to_json, Hex};
use crate::process_details::AddressType;
use iced_x86::{CodeSize, Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, Mnemonic, OpKind, Register};
use process_memory::{CopyAddress, DataMember, Memory, Pid, ProcessHandle, TryIntoProcessHandle};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use winapi::shared::minwindef::FALSE;
use winapi::shared::winerror::ERROR_SEM_TIMEOUT;
use winapi::um::debugapi::{ContinueDebugEvent, DebugActiveProcess, DebugActiveProcessStop, WaitForDebugEvent};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::handleapi::CloseHandle;
use winapi::um::minwinbase::{
    CREATE_PROCESS_DEBUG_EVENT, CREATE_THREAD_DEBUG_EVENT, DEBUG_EVENT, EXCEPTION_DEBUG_EVENT, EXIT_PROCESS_DEBUG_EVENT,
    EXIT_THREAD_DEBUG_EVENT, LOAD_DLL_DEBUG_EVENT,
};
use winapi::um::processthreadsapi::{GetThreadContext, ResumeThread, SetThreadContext, SuspendThread};
use winapi::um::winbase::{DebugSetProcessKillOnExit, Wow64GetThreadContext, Wow64SetThreadContext};
use winapi::um::winnt::{
    CONTEXT, CONTEXT_DEBUG_REGISTERS, CONTEXT_FULL, DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, HANDLE, WOW64_CONTEXT,
    WOW64_CONTEXT_DEBUG_REGISTERS, WOW64_CONTEXT_FULL,
};
use winapi::um::wow64apiset::IsWow64Process;

// EFLAGS.RF
const RESUME_FLAG: u32 = 0x1_0000;
// Register sets printed per instruction by `watch-raw --exec`
const MAX_REGISTER_SETS: usize = 5;
// x86 has 4 debug address registers
pub const MAX_WATCHES: usize = 4;
const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
// The same, raised by a 32-bit game's code under WOW64
const STATUS_WX86_SINGLE_STEP: u32 = 0x4000_001E;
const STATUS_WX86_BREAKPOINT: u32 = 0x4000_001F;
// Longest x86 instruction
const MAX_INSTRUCTION_LENGTH: usize = 15;
// Witnesses suggested per value
const MAX_WITNESSES: usize = 8;
// How much of the stack to search for return addresses, and how many callers to keep
const STACK_SCAN_SIZE: usize = 0x1000;
const MAX_CALLERS: usize = 8;
// Caller chains printed per instruction by `watch-raw`
const MAX_CALLER_CHAINS: usize = 3;

#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// What sets a hardware breakpoint off
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    // Reading or writing the address
    Access,
    Write,
    // Running the instruction at the address
    Execute,
}

impl Breakpoint {
    // Dr7's RW bits
    fn bits(self) -> u64 {
        match self {
            Breakpoint::Access => 0b11,
            Breakpoint::Write => 0b01,
            Breakpoint::Execute => 0b00,
        }
    }
}

/// A thread's registers. For a 32-bit game these are the WOW64 ones, which are what its code sees: the native
/// context of a 32-bit thread is the 64-bit layer it runs on.
enum Context {
    Native(AlignedContext),
    Wow64(WOW64_CONTEXT),
}

/// Whether a process is a 32-bit one running on 64-bit Windows
fn is_wow64(handle: ProcessHandle) -> bool {
    let mut wow64 = FALSE;
    unsafe { IsWow64Process(handle, &mut wow64) != FALSE && wow64 != FALSE }
}

impl Context {
    /// The debug registers, or with `full` all of them
    fn get(thread: HANDLE, wow64: bool, full: bool) -> Option<Context> {
        if wow64 {
            let mut context: WOW64_CONTEXT = unsafe { std::mem::zeroed() };
            context.ContextFlags =
                if full { WOW64_CONTEXT_FULL | WOW64_CONTEXT_DEBUG_REGISTERS } else { WOW64_CONTEXT_DEBUG_REGISTERS };
            (unsafe { Wow64GetThreadContext(thread, &mut context) } != FALSE).then(|| Context::Wow64(context))
        } else {
            let mut context: AlignedContext = unsafe { std::mem::zeroed() };
            context.0.ContextFlags =
                if full { CONTEXT_FULL | CONTEXT_DEBUG_REGISTERS } else { CONTEXT_DEBUG_REGISTERS };
            (unsafe { GetThreadContext(thread, &mut context.0) } != FALSE).then(|| Context::Native(context))
        }
    }

    fn set(&self, thread: HANDLE) -> bool {
        match self {
            Context::Native(c) => unsafe { SetThreadContext(thread, &c.0) != FALSE },
            Context::Wow64(c) => unsafe { Wow64SetThreadContext(thread, c) != FALSE },
        }
    }

    fn bitness(&self) -> u32 {
        match self {
            Context::Native(_) => 64,
            Context::Wow64(_) => 32,
        }
    }

    fn ip(&self) -> usize {
        match self {
            Context::Native(c) => c.0.Rip as usize,
            Context::Wow64(c) => c.Eip as usize,
        }
    }

    fn sp(&self) -> usize {
        match self {
            Context::Native(c) => c.0.Rsp as usize,
            Context::Wow64(c) => c.Esp as usize,
        }
    }

    fn dr6(&self) -> u64 {
        match self {
            Context::Native(c) => c.0.Dr6,
            Context::Wow64(c) => c.Dr6 as u64,
        }
    }

    /// Sets Dr0-3 and Dr7, and clears Dr6
    fn set_debug_registers(&mut self, addresses: [u64; MAX_WATCHES], dr7: impl FnOnce(u64) -> u64) {
        match self {
            Context::Native(c) => {
                let c = &mut c.0;
                c.Dr0 = addresses[0];
                c.Dr1 = addresses[1];
                c.Dr2 = addresses[2];
                c.Dr3 = addresses[3];
                c.Dr7 = dr7(c.Dr7);
                c.Dr6 = 0;
            }
            Context::Wow64(c) => {
                c.Dr0 = addresses[0] as u32;
                c.Dr1 = addresses[1] as u32;
                c.Dr2 = addresses[2] as u32;
                c.Dr3 = addresses[3] as u32;
                c.Dr7 = dr7(c.Dr7 as u64) as u32;
                c.Dr6 = 0;
            }
        }
    }

    /// Lets the thread carry on past an execute breakpoint, rather than stopping at it again
    fn set_resume_flag(&mut self) {
        match self {
            Context::Native(c) => c.0.EFlags |= RESUME_FLAG,
            Context::Wow64(c) => c.EFlags |= RESUME_FLAG,
        }
    }

    fn registers(&self) -> String {
        match self {
            Context::Native(c) => {
                let c = &c.0;
                format!(
                    "rax={:X} rbx={:X} rcx={:X} rdx={:X} rsi={:X} rdi={:X} rbp={:X} rsp={:X} r8={:X} r9={:X}",
                    c.Rax, c.Rbx, c.Rcx, c.Rdx, c.Rsi, c.Rdi, c.Rbp, c.Rsp, c.R8, c.R9
                )
            }
            Context::Wow64(c) => format!(
                "eax={:X} ebx={:X} ecx={:X} edx={:X} esi={:X} edi={:X} ebp={:X} esp={:X}",
                c.Eax, c.Ebx, c.Ecx, c.Edx, c.Esi, c.Edi, c.Ebp, c.Esp
            ),
        }
    }

    fn clear_dr6(&mut self) {
        match self {
            Context::Native(c) => c.0.Dr6 = 0,
            Context::Wow64(c) => c.Dr6 = 0,
        }
    }

    fn register(&self, register: Register) -> Option<u64> {
        // Segment bases are 0 (flat) for everything but fs/gs, which aren't used for game data
        if matches!(register, Register::ES | Register::CS | Register::SS | Register::DS) {
            return Some(0);
        }
        Some(match self {
            Context::Native(c) => {
                let c = &c.0;
                match register {
                    Register::RAX => c.Rax,
                    Register::RBX => c.Rbx,
                    Register::RCX => c.Rcx,
                    Register::RDX => c.Rdx,
                    Register::RSI => c.Rsi,
                    Register::RDI => c.Rdi,
                    Register::RBP => c.Rbp,
                    Register::RSP => c.Rsp,
                    Register::R8 => c.R8,
                    Register::R9 => c.R9,
                    Register::R10 => c.R10,
                    Register::R11 => c.R11,
                    Register::R12 => c.R12,
                    Register::R13 => c.R13,
                    Register::R14 => c.R14,
                    Register::R15 => c.R15,
                    _ => return None,
                }
            }
            Context::Wow64(c) => u64::from(match register {
                Register::EAX => c.Eax,
                Register::EBX => c.Ebx,
                Register::ECX => c.Ecx,
                Register::EDX => c.Edx,
                Register::ESI => c.Esi,
                Register::EDI => c.Edi,
                Register::EBP => c.Ebp,
                Register::ESP => c.Esp,
                _ => return None,
            }),
        })
    }
}

/// Points debug registers 0..n at the addresses (1 byte each, break on read or write, only on write, or on
/// running the instruction there), or clears them. They're set in the native context even for a 32-bit game, as the
/// processor's debug registers aren't per mode.
fn set_watches(thread: HANDLE, addresses: &[usize], kind: Breakpoint) -> bool {
    let mut context = match Context::get(thread, false, false) {
        Some(c) => c,
        None => return false,
    };
    let mut values = [0_u64; MAX_WATCHES];
    let mut enable = 0;
    for (i, address) in addresses.iter().enumerate().take(MAX_WATCHES) {
        values[i] = *address as u64;
        // Local enable, RW (see `Breakpoint`), LEN = 00 (1 byte, and the only length for execute)
        enable |= 1 << (i * 2);
        enable |= kind.bits() << (16 + i * 4);
    }
    context.set_debug_registers(values, |dr7| dr7 & !0xFFFF_00FF | enable);
    context.set(thread)
}

/// An instruction's displacement, sign extended (iced zero extends 32-bit addressing's)
fn displacement(instruction: &Instruction) -> i64 {
    if instruction.code_size() == CodeSize::Code64 {
        instruction.memory_displacement64() as i64
    } else {
        instruction.memory_displacement32() as i32 as i64
    }
}

/// Data breakpoints trap after the access, so find the instruction ending at the instruction pointer whose memory
/// operand covers `watched`, using the registers at the time of the trap. After a call through memory (e.g. the
/// import table) the instruction pointer is in the callee, so the call ends at the return address instead.
fn accessing_instruction(handle: ProcessHandle, c: &Context, watched: usize) -> Option<Instruction> {
    instruction_ending_at(handle, c, c.ip(), watched).or_else(|| {
        let mut bytes = [0_u8; 8];
        let slot = c.bitness() as usize / 8;
        handle.copy_address(c.sp(), &mut bytes[..slot]).ok()?;
        let returns_to = u64::from_le_bytes(bytes) as usize;
        instruction_ending_at(handle, c, returns_to, watched).filter(|i| i.mnemonic() == Mnemonic::Call)
    })
}

fn instruction_at(handle: ProcessHandle, c: &Context, ip: usize) -> Option<Instruction> {
    let mut bytes = [0_u8; MAX_INSTRUCTION_LENGTH];
    handle.copy_address(ip, &mut bytes).ok()?;
    let instruction = Decoder::with_ip(c.bitness(), &bytes, ip as u64, DecoderOptions::NONE).decode();
    (!instruction.is_invalid()).then(|| instruction)
}

fn instruction_ending_at(handle: ProcessHandle, c: &Context, ip: usize, watched: usize) -> Option<Instruction> {
    let mut bytes = [0_u8; MAX_INSTRUCTION_LENGTH];
    handle.copy_address(ip - MAX_INSTRUCTION_LENGTH, &mut bytes).ok()?;
    (1..=MAX_INSTRUCTION_LENGTH).rev().find_map(|length| {
        let start = ip - length;
        let mut decoder = Decoder::with_ip(
            c.bitness(),
            &bytes[MAX_INSTRUCTION_LENGTH - length..],
            start as u64,
            DecoderOptions::NONE,
        );
        let instruction = decoder.decode();
        if instruction.is_invalid() || instruction.len() != length {
            return None;
        }
        let operand = (0..instruction.op_count()).find(|i| instruction.op_kind(*i) == OpKind::Memory)?;
        let address = instruction.virtual_address(operand, 0, |r, _, _| c.register(r))? as usize;
        let size = instruction.memory_size().size().max(1);
        if address <= watched && watched < address + size {
            Some(instruction)
        } else {
            None
        }
    })
}

pub struct Hit {
    pub count: usize,
    pub text: String,
    pub displacement: i64,
    // Return addresses found on the stack (as RVAs), most recent first, and how often each chain was seen
    callers: BTreeMap<Vec<usize>, usize>,
    // For execute breakpoints, the registers each time, and how often each set was seen
    registers: BTreeMap<String, usize>,
    // With `--dump`, the memory at the register the first few times (its value, the bytes)
    dumps: Vec<(u64, Vec<u8>)>,
}

/// `watch-raw --exec --dump <register>:<length>`: copy the memory a register points at when the breakpoint is hit
#[derive(Clone, Copy)]
pub struct Dump {
    register: Register,
    length: usize,
}

// Dumps kept per instruction
const MAX_DUMPS: usize = 3;

impl Dump {
    pub fn parse(text: &str) -> Result<Dump> {
        let (name, length) = text.split_once(':').context("--dump takes <register>:<length in hex>")?;
        let register = match name.to_ascii_lowercase().as_str() {
            "eax" => Register::EAX,
            "ebx" => Register::EBX,
            "ecx" => Register::ECX,
            "edx" => Register::EDX,
            "esi" => Register::ESI,
            "edi" => Register::EDI,
            "ebp" => Register::EBP,
            "esp" => Register::ESP,
            "rax" => Register::RAX,
            "rbx" => Register::RBX,
            "rcx" => Register::RCX,
            "rdx" => Register::RDX,
            "rsi" => Register::RSI,
            "rdi" => Register::RDI,
            "rbp" => Register::RBP,
            "rsp" => Register::RSP,
            "r8" => Register::R8,
            "r9" => Register::R9,
            _ => bail!("Unknown register {}", name),
        };
        Ok(Dump { register, length: parse_hex(length)? })
    }
}

/// A best guess at the call stack: values on the stack that point just after a call instruction in `code`.
/// Without unwind info this can include stale return addresses, but the first few are usually right.
fn stack_callers(handle: ProcessHandle, c: &Context, code: (usize, usize), base_addr: usize) -> Vec<usize> {
    // Near the top of the stack a long read runs off the end, so fall back to shorter ones
    let stack = match (0..4)
        .map(|shift| vec![0_u8; STACK_SCAN_SIZE >> shift])
        .find_map(|mut stack| handle.copy_address(c.sp(), &mut stack).ok().map(|_| stack))
    {
        Some(stack) => stack,
        None => return vec![],
    };
    let slot = c.bitness() as usize / 8;
    let mut callers = vec![];
    for value in stack.chunks_exact(slot).map(|b| {
        let mut bytes = [0_u8; 8];
        bytes[..slot].copy_from_slice(b);
        u64::from_le_bytes(bytes) as usize
    }) {
        if value < code.0 + 8 || value >= code.1 {
            continue;
        }
        let mut before = [0_u8; 7];
        if handle.copy_address(value - before.len(), &mut before).is_err() {
            continue;
        }
        // call rel32 / call [rip+disp32] (or [disp32] on x86) / call [reg+disp8] / call reg
        let is_call = before[2] == 0xE8
            || (before[1] == 0xFF && before[2] & 0x38 == 0x10)
            || (before[4] == 0xFF && before[5] & 0x38 == 0x10)
            || (before[5] == 0xFF && before[6] & 0xF8 == 0xD0);
        if is_call {
            callers.push(value - base_addr);
            if callers.len() == MAX_CALLERS {
                break;
            }
        }
    }
    callers
}

/// Records which instructions access each address, as RVAs relative to `base_addr`, and with `code_range` (the
/// range of the game's code) which functions called them
pub fn record(
    pid: Pid,
    handle: ProcessHandle,
    base_addr: usize,
    addresses: &[usize],
    seconds: u64,
    kind: Breakpoint,
    code_range: Option<(usize, usize)>,
    dump: Option<Dump>,
) -> Result<Vec<BTreeMap<usize, Hit>>> {
    let wow64 = is_wow64(handle);
    if unsafe { DebugActiveProcess(pid) } == FALSE {
        bail!("Could not attach as a debugger (error {})", unsafe { GetLastError() });
    }
    // Never take the game down with us
    unsafe { DebugSetProcessKillOnExit(FALSE) };
    println!("Watching for {} seconds...", seconds);

    let mut threads: HashMap<u32, HANDLE> = HashMap::new();
    let mut hits: Vec<BTreeMap<usize, Hit>> = (0..addresses.len()).map(|_| BTreeMap::new()).collect();
    let mut formatter = IntelFormatter::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut detaching = false;
    let mut armed = 0;
    let mut traps = 0;
    let mut exited = false;
    let mut attached = false;
    // Once the time is up, clear the breakpoints and keep handling events until they stop,
    // since detaching with a breakpoint event still pending would crash the game
    let mut quiet_since: Option<Instant> = None;

    loop {
        if !detaching && Instant::now() >= deadline {
            detaching = true;
            for thread in threads.values() {
                unsafe { SuspendThread(*thread) };
                set_watches(*thread, &[], kind);
                unsafe { ResumeThread(*thread) };
            }
        }
        if detaching && quiet_since.map_or(false, |t| t.elapsed() > Duration::from_millis(500)) {
            break;
        }

        let mut event: DEBUG_EVENT = unsafe { std::mem::zeroed() };
        if unsafe { WaitForDebugEvent(&mut event, 100) } == FALSE {
            if unsafe { GetLastError() } == ERROR_SEM_TIMEOUT && detaching && quiet_since.is_none() {
                quiet_since = Some(Instant::now());
            }
            continue;
        }
        quiet_since = None;

        let mut status = DBG_CONTINUE;
        match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let info = unsafe { event.u.CreateProcessInfo() };
                unsafe { CloseHandle(info.hFile) };
                threads.insert(event.dwThreadId, info.hThread);
                armed += set_watches(info.hThread, if detaching { &[] } else { &addresses }, kind) as usize;
            }
            CREATE_THREAD_DEBUG_EVENT => {
                let thread = unsafe { event.u.CreateThread() }.hThread;
                threads.insert(event.dwThreadId, thread);
                armed += set_watches(thread, if detaching { &[] } else { &addresses }, kind) as usize;
            }
            EXIT_THREAD_DEBUG_EVENT => {
                threads.remove(&event.dwThreadId);
            }
            LOAD_DLL_DEBUG_EVENT => {
                unsafe { CloseHandle(event.u.LoadDll().hFile) };
            }
            EXIT_PROCESS_DEBUG_EVENT => exited = true,
            EXCEPTION_DEBUG_EVENT => {
                let code = unsafe { event.u.Exception() }.ExceptionRecord.ExceptionCode;
                let is_breakpoint = code == STATUS_BREAKPOINT || code == STATUS_WX86_BREAKPOINT;
                if code == STATUS_SINGLE_STEP || code == STATUS_WX86_SINGLE_STEP {
                    traps += 1;
                    if let Some(thread) = threads.get(&event.dwThreadId) {
                        // Which breakpoint it was is in the native context, and the registers the game's code
                        // sees in the WOW64 one for a 32-bit game
                        let debug = Context::get(*thread, false, false);
                        let context = Context::get(*thread, wow64, true);
                        if let (Some(mut debug), Some(mut context)) = (debug, context) {
                            for (i, address) in addresses.iter().enumerate() {
                                if debug.dr6() & (1 << i) == 0 {
                                    continue;
                                }
                                // An execute breakpoint stops before the instruction, a data one after it
                                let instruction = if kind == Breakpoint::Execute {
                                    instruction_at(handle, &context, context.ip())
                                } else {
                                    accessing_instruction(handle, &context, *address)
                                };
                                if let Some(instruction) = instruction {
                                    let rva = instruction.ip() as usize - base_addr;
                                    let hit = hits[i].entry(rva).or_insert_with(|| {
                                        let mut text = String::new();
                                        formatter.format(&instruction, &mut text);
                                        Hit {
                                            count: 0,
                                            text,
                                            displacement: displacement(&instruction),
                                            callers: BTreeMap::new(),
                                            registers: BTreeMap::new(),
                                            dumps: vec![],
                                        }
                                    });
                                    hit.count += 1;
                                    if kind == Breakpoint::Execute {
                                        *hit.registers.entry(context.registers()).or_insert(0) += 1;
                                    }
                                    if let Some(dump) = dump.filter(|_| hit.dumps.len() < MAX_DUMPS) {
                                        if let Some(at) = context.register(dump.register) {
                                            let mut bytes = vec![0_u8; dump.length];
                                            if handle.copy_address(at as usize, &mut bytes).is_ok() {
                                                hit.dumps.push((at, bytes));
                                            }
                                        }
                                    }
                                    if let Some(code_range) = code_range {
                                        *hit.callers.entry(stack_callers(handle, &context, code_range, base_addr)).or_insert(0) += 1;
                                    }
                                }
                            }
                            if kind == Breakpoint::Execute {
                                context.clear_dr6();
                                context.set_resume_flag();
                                context.set(*thread);
                            } else {
                                debug.clear_dr6();
                                debug.set(*thread);
                            }
                        }
                    }
                } else if is_breakpoint && !attached {
                    // The breakpoint Windows raises once attached; every thread is known by now
                    attached = true;
                    for thread in threads.values() {
                        armed += set_watches(*thread, &addresses, kind) as usize;
                    }
                } else if !is_breakpoint {
                    // The game's own exceptions are its business
                    status = DBG_EXCEPTION_NOT_HANDLED;
                }
            }
            _ => {}
        }
        unsafe { ContinueDebugEvent(event.dwProcessId, event.dwThreadId, status) };
        if exited {
            break;
        }
    }
    if !exited {
        unsafe { DebugActiveProcessStop(pid) };
    }
    println!(
        "Detached ({} threads, breakpoints set on {}, {} traps)\n",
        threads.len(),
        armed,
        traps
    );

    Ok(hits)
}

fn parse_hex(text: &str) -> Result<usize> {
    usize::from_str_radix(text.trim_start_matches("0x"), 16).with_context(|| format!("{}", text))
}

// An absolute address in hex, or one relative to the main module with a leading `+`
fn parse_address(text: &str, base_addr: usize) -> Result<usize> {
    match text.strip_prefix('+') {
        Some(rva) => Ok(base_addr + parse_hex(rva)?),
        None => parse_hex(text),
    }
}

/// `watch-raw <pid> <address>[,<address>...]`: record what accesses (or writes, or with `Breakpoint::Execute` runs) any address in a
/// process, and for the main module's code which functions it was called from (and for an instruction run, the
/// registers then). Addresses are printed relative to the main module.
pub fn watch_raw(pid: &str, address: &str, seconds: u64, delay: u64, kind: Breakpoint, dump: Option<Dump>) -> Result<()> {
    let pid: Pid = pid.parse().context("Bad PID")?;
    let handle = pid.try_into_process_handle()?;
    let base_addr = find_process::get_base_address(pid) as usize;
    let image_size = find_process::get_image_size(handle, base_addr)?;
    // Up to 4, separated by commas
    let addresses: Vec<usize> = address.split(',').map(|a| parse_address(a, base_addr)).collect::<Result<_, _>>()?;
    if addresses.is_empty() || addresses.len() > MAX_WATCHES {
        bail!("Give between 1 and {} addresses", MAX_WATCHES);
    }
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let hits = record(pid, handle, base_addr, &addresses, seconds, kind, Some((base_addr, base_addr + image_size)), dump)?;
    let mut by_count: Vec<(&usize, &Hit)> = hits.iter().flatten().collect();
    by_count.sort_by_key(|(rva, hit)| (std::cmp::Reverse(hit.count), **rva));
    for (rva, hit) in by_count {
        println!("    +{:#X} {:>6}x  {}", rva, hit.count, hit.text);
        print_callers(hit);
        let mut sets: Vec<(&String, &usize)> = hit.registers.iter().collect();
        sets.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (registers, count) in sets.into_iter().take(MAX_REGISTER_SETS) {
            println!("        {:>6}x  {}", count, registers);
        }
        for (at, bytes) in &hit.dumps {
            print_dump(*at, bytes);
        }
    }
    Ok(())
}

// 16 bytes a row: as u32s, then as floats
fn print_dump(at: u64, bytes: &[u8]) {
    println!("        memory at {:#X}:", at);
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let words: Vec<u32> = chunk.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let hex: Vec<String> = words.iter().map(|w| format!("{:08X}", w)).collect();
        let floats: Vec<String> = words.iter().map(|w| format!("{:>10.4}", f32::from_bits(*w))).collect();
        println!("          +{:03X} {}   {}", row * 16, hex.join(" "), floats.join(" "));
    }
}

fn print_callers(hit: &Hit) {
    let mut chains: Vec<(&Vec<usize>, &usize)> = hit.callers.iter().collect();
    chains.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (chain, count) in chains.into_iter().take(MAX_CALLER_CHAINS) {
        let chain: Vec<String> = chain.iter().map(|rva| format!("+{:#X}", rva)).collect();
        println!("        {:>6}x  called from {}", count, chain.join(" <- "));
    }
}

/// `disasm <pid> <address> [count]`: print instructions from a running process's memory
pub fn disasm(pid: &str, address: &str, count: &str) -> Result<()> {
    let pid: Pid = pid.parse().context("Bad PID")?;
    let handle = pid.try_into_process_handle()?;
    let base_addr = find_process::get_base_address(pid) as usize;
    let address = parse_address(address, base_addr)?;
    let count: usize = count.parse().context("Bad instruction count")?;
    let mut bytes = vec![0_u8; count * MAX_INSTRUCTION_LENGTH];
    handle.copy_address(address, &mut bytes)?;
    let bitness = if is_wow64(handle) { 32 } else { 64 };
    let mut decoder = Decoder::with_ip(bitness, &bytes, address as u64, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    let mut text = String::new();
    for instruction in decoder.iter().take(count) {
        text.clear();
        formatter.format(&instruction, &mut text);
        println!("+{:#X}  {}", instruction.ip() as usize - base_addr, text);
    }
    Ok(())
}

pub fn watch(names: &[&str], version: Option<String>, seconds: u64, delay: u64, write_only: bool) -> Result<()> {
    let kind = if write_only { Breakpoint::Write } else { Breakpoint::Access };
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let (pid, handle, base_addr, details) = find_process::find_process(known_process_details(), version)
        .context("No known game version is running (pass --version to force one from process_details.rs)")?;
    println!("Found {} {}", details.name, details.version.version);

    let all: BTreeMap<String, AddressType> = details
        .address_offsets
        .keys()
        .map(|t| (format!("{:?}", t), *t))
        .collect();
    let mut watched: Vec<(AddressType, usize)> = vec![];
    for name in names {
        let address_type = *all
            .get(*name)
            .with_context(|| format!("Unknown address type {}. Known: {:?}", name, all.keys().collect::<Vec<_>>()))?;
        let offsets = &details.address_offsets[&address_type];
        let mut with_base = offsets.clone();
        with_base[0] += base_addr;
        let address = DataMember::<u8>::new_offset(handle, with_base)
            .set_arch(details.arch)
            .get_offset()
            .with_context(|| format!("{:?} {} doesn't resolve", address_type, format_offsets(offsets)))?;
        watched.push((address_type, address));
    }
    if watched.is_empty() || watched.len() > MAX_WATCHES {
        bail!("Give between 1 and {} address types to watch", MAX_WATCHES);
    }
    let addresses: Vec<usize> = watched.iter().map(|(_, a)| *a).collect();

    let image_size = find_process::get_image_size(handle, base_addr)?;
    let hits = record(pid, handle, base_addr, &addresses, seconds, kind, Some((base_addr, base_addr + image_size)), None)?;

    let mut witnesses: BTreeMap<AddressType, Vec<Hex>> = BTreeMap::new();
    for ((address_type, address), hits) in watched.iter().zip(&hits) {
        let last_offset = *details.address_offsets[address_type].last().unwrap() as i64;
        println!("{:?} at {:#X}:", address_type, address);
        if hits.is_empty() {
            println!("    not accessed");
        }
        let mut by_count: Vec<(&usize, &Hit)> = hits.iter().collect();
        by_count.sort_by_key(|(rva, hit)| (std::cmp::Reverse(hit.count), **rva));
        for (rva, hit) in &by_count {
            println!("    {:#010X} {:>6}x  {}", rva, hit.count, hit.text);
            print_callers(hit);
        }
        // The best witnesses access the field with the same offset as the chain (so they use the
        // same object pointer), and run often (so they're the main code path, not a one-off)
        let mut best: Vec<(&usize, &Hit)> = by_count
            .iter()
            .filter(|(_, hit)| hit.displacement != 0)
            .cloned()
            .collect();
        best.sort_by_key(|(rva, hit)| (hit.displacement != last_offset, std::cmp::Reverse(hit.count), **rva));
        best.truncate(MAX_WITNESSES);
        if !best.is_empty() {
            witnesses.insert(*address_type, best.iter().map(|(rva, _)| Hex(**rva)).collect());
        }
    }
    if !witnesses.is_empty() {
        println!(
            "\n// Instructions that access each value, for `derive` to match in new versions. Add these to\n\
             // the \"witnesses\" of {} in the offsets file:",
            details.version.version
        );
        println!("\"witnesses\": {}", to_json(&witnesses));
    }
    Ok(())
}
