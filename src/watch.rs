//! Finds the instructions that access a value ("witnesses") by attaching as a debugger for a few
//! seconds and putting hardware breakpoints on it. This finds code that a static trace can't reach,
//! e.g. fields read through objects found in lists. `derive` then matches these instructions in a
//! new build to find where the field moved to.

use crate::derive::format_offsets;
use crate::find_process;
use crate::offsets_file::{known_process_details, to_json, Hex};
use crate::process_details::AddressType;
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, OpKind, Register};
use process_memory::{CopyAddress, DataMember, Memory, Pid, ProcessHandle, TryIntoProcessHandle};
use std::collections::{BTreeMap, HashMap};
use std::convert::TryInto;
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
use winapi::um::winbase::DebugSetProcessKillOnExit;
use winapi::um::winnt::{CONTEXT, CONTEXT_DEBUG_REGISTERS, CONTEXT_FULL, DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, HANDLE};

// x86 has 4 debug address registers
const MAX_WATCHES: usize = 4;
const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
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

fn thread_context(thread: HANDLE, flags: u32) -> Option<AlignedContext> {
    let mut context: AlignedContext = unsafe { std::mem::zeroed() };
    context.0.ContextFlags = flags;
    if unsafe { GetThreadContext(thread, &mut context.0) } == FALSE {
        None
    } else {
        Some(context)
    }
}

/// Points debug registers 0..n at the addresses (1 byte each, break on read or write, or only on
/// write), or clears them
fn set_watches(thread: HANDLE, addresses: &[usize], write_only: bool) -> bool {
    let mut context = match thread_context(thread, CONTEXT_DEBUG_REGISTERS) {
        Some(c) => c,
        None => return false,
    };
    let c = &mut context.0;
    let mut dr7 = c.Dr7 & !0xFFFF_00FF;
    let mut values = [0_u64; MAX_WATCHES];
    for (i, address) in addresses.iter().enumerate().take(MAX_WATCHES) {
        values[i] = *address as u64;
        // Local enable, RW = 11 (read or write) or 01 (write), LEN = 00 (1 byte)
        dr7 |= 1 << (i * 2);
        dr7 |= if write_only { 0b01 } else { 0b11 } << (16 + i * 4);
    }
    c.Dr0 = values[0];
    c.Dr1 = values[1];
    c.Dr2 = values[2];
    c.Dr3 = values[3];
    c.Dr7 = dr7;
    c.Dr6 = 0;
    unsafe { SetThreadContext(thread, &context.0) != FALSE }
}

fn register_value(c: &CONTEXT, register: Register) -> Option<u64> {
    Some(match register {
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
        // Segment bases are 0 in 64-bit mode, except fs/gs which aren't used for game data
        Register::ES | Register::CS | Register::SS | Register::DS => 0,
        _ => return None,
    })
}

/// Data breakpoints trap after the access, so find the instruction ending at `rip` whose memory
/// operand covers `watched`, using the registers at the time of the trap.
fn accessing_instruction(handle: ProcessHandle, c: &CONTEXT, watched: usize) -> Option<Instruction> {
    let rip = c.Rip as usize;
    let mut bytes = [0_u8; MAX_INSTRUCTION_LENGTH];
    handle.copy_address(rip - MAX_INSTRUCTION_LENGTH, &mut bytes).ok()?;
    (1..=MAX_INSTRUCTION_LENGTH).rev().find_map(|length| {
        let start = rip - length;
        let mut decoder = Decoder::with_ip(
            64,
            &bytes[MAX_INSTRUCTION_LENGTH - length..],
            start as u64,
            DecoderOptions::NONE,
        );
        let instruction = decoder.decode();
        if instruction.is_invalid() || instruction.len() != length {
            return None;
        }
        let operand = (0..instruction.op_count()).find(|i| instruction.op_kind(*i) == OpKind::Memory)?;
        let address = instruction.virtual_address(operand, 0, |r, _, _| register_value(c, r))? as usize;
        let size = instruction.memory_size().size().max(1);
        if address <= watched && watched < address + size {
            Some(instruction)
        } else {
            None
        }
    })
}

struct Hit {
    count: usize,
    text: String,
    displacement: i64,
    // Return addresses found on the stack (as RVAs), most recent first, and how often each chain was seen
    callers: BTreeMap<Vec<usize>, usize>,
}

/// A best guess at the call stack: values on the stack that point just after a call instruction in `code`.
/// Without unwind info this can include stale return addresses, but the first few are usually right.
fn stack_callers(handle: ProcessHandle, c: &CONTEXT, code: (usize, usize), base_addr: usize) -> Vec<usize> {
    // Near the top of the stack a long read runs off the end, so fall back to shorter ones
    let stack = match (0..4)
        .map(|shift| vec![0_u8; STACK_SCAN_SIZE >> shift])
        .find_map(|mut stack| handle.copy_address(c.Rsp as usize, &mut stack).ok().map(|_| stack))
    {
        Some(stack) => stack,
        None => return vec![],
    };
    let mut callers = vec![];
    for value in stack.chunks_exact(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize) {
        if value < code.0 + 8 || value >= code.1 {
            continue;
        }
        let mut before = [0_u8; 7];
        if handle.copy_address(value - before.len(), &mut before).is_err() {
            continue;
        }
        // call rel32 / call [rip+disp32] / call [reg+disp8] / call reg
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
fn record(
    pid: Pid,
    handle: ProcessHandle,
    base_addr: usize,
    addresses: &[usize],
    seconds: u64,
    write_only: bool,
    code_range: Option<(usize, usize)>,
) -> Result<Vec<BTreeMap<usize, Hit>>, String> {
    if unsafe { DebugActiveProcess(pid) } == FALSE {
        return Err(format!("Could not attach as a debugger (error {})", unsafe { GetLastError() }));
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
                set_watches(*thread, &[], write_only);
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
                armed += set_watches(info.hThread, if detaching { &[] } else { &addresses }, write_only) as usize;
            }
            CREATE_THREAD_DEBUG_EVENT => {
                let thread = unsafe { event.u.CreateThread() }.hThread;
                threads.insert(event.dwThreadId, thread);
                armed += set_watches(thread, if detaching { &[] } else { &addresses }, write_only) as usize;
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
                if code == STATUS_SINGLE_STEP {
                    traps += 1;
                    if let Some(thread) = threads.get(&event.dwThreadId) {
                        if let Some(mut context) =
                            thread_context(*thread, CONTEXT_FULL | CONTEXT_DEBUG_REGISTERS)
                        {
                            for (i, address) in addresses.iter().enumerate() {
                                if context.0.Dr6 & (1 << i) == 0 {
                                    continue;
                                }
                                if let Some(instruction) = accessing_instruction(handle, &context.0, *address) {
                                    let rva = instruction.ip() as usize - base_addr;
                                    let hit = hits[i].entry(rva).or_insert_with(|| {
                                        let mut text = String::new();
                                        formatter.format(&instruction, &mut text);
                                        Hit {
                                            count: 0,
                                            text,
                                            displacement: instruction.memory_displacement64() as i64,
                                            callers: BTreeMap::new(),
                                        }
                                    });
                                    hit.count += 1;
                                    if let Some(code_range) = code_range {
                                        *hit.callers.entry(stack_callers(handle, &context.0, code_range, base_addr)).or_insert(0) += 1;
                                    }
                                }
                            }
                            context.0.Dr6 = 0;
                            unsafe { SetThreadContext(*thread, &context.0) };
                        }
                    }
                } else if code == STATUS_BREAKPOINT && !attached {
                    // The breakpoint Windows raises once attached; every thread is known by now
                    attached = true;
                    for thread in threads.values() {
                        armed += set_watches(*thread, &addresses, write_only) as usize;
                    }
                } else if code != STATUS_BREAKPOINT {
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

fn parse_hex(text: &str) -> Result<usize, String> {
    usize::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|e| format!("{}: {}", text, e))
}

// An absolute address in hex, or one relative to the main module with a leading `+`
fn parse_address(text: &str, base_addr: usize) -> Result<usize, String> {
    match text.strip_prefix('+') {
        Some(rva) => Ok(base_addr + parse_hex(rva)?),
        None => parse_hex(text),
    }
}

/// `watch-raw <pid> <address>`: record what accesses (or with `write_only` writes) any address in a 64-bit
/// process, and for the main module's code which functions it was called from. Addresses are printed
/// relative to the main module.
pub fn watch_raw(pid: &str, address: &str, seconds: u64, delay: u64, write_only: bool) -> Result<(), String> {
    let pid: Pid = pid.parse().map_err(|_| "Bad PID")?;
    let handle = pid.try_into_process_handle().map_err(|e| e.to_string())?;
    let base_addr = find_process::get_base_address(pid) as usize;
    let image_size = find_process::get_image_size(handle, base_addr).map_err(|e| e.to_string())?;
    let address = parse_address(address, base_addr)?;
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let hits = record(pid, handle, base_addr, &[address], seconds, write_only, Some((base_addr, base_addr + image_size)))?;
    let mut by_count: Vec<(&usize, &Hit)> = hits[0].iter().collect();
    by_count.sort_by_key(|(rva, hit)| (std::cmp::Reverse(hit.count), **rva));
    for (rva, hit) in by_count {
        println!("    +{:#X} {:>6}x  {}", rva, hit.count, hit.text);
        print_callers(hit);
    }
    Ok(())
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
pub fn disasm(pid: &str, address: &str, count: &str) -> Result<(), String> {
    let pid: Pid = pid.parse().map_err(|_| "Bad PID")?;
    let handle = pid.try_into_process_handle().map_err(|e| e.to_string())?;
    let base_addr = find_process::get_base_address(pid) as usize;
    let address = parse_address(address, base_addr)?;
    let count: usize = count.parse().map_err(|_| "Bad instruction count")?;
    let mut bytes = vec![0_u8; count * MAX_INSTRUCTION_LENGTH];
    handle.copy_address(address, &mut bytes).map_err(|e| e.to_string())?;
    let mut decoder = Decoder::with_ip(64, &bytes, address as u64, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    let mut text = String::new();
    for instruction in decoder.iter().take(count) {
        text.clear();
        formatter.format(&instruction, &mut text);
        println!("+{:#X}  {}", instruction.ip() as usize - base_addr, text);
    }
    Ok(())
}

pub fn watch(names: &[&str], version: Option<String>, seconds: u64, delay: u64, write_only: bool) -> Result<(), String> {
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let (pid, handle, base_addr, details) = find_process::find_process(known_process_details(), version)
        .ok_or("No known game version is running (pass --version to force one from process_details.rs)")?;
    if !matches!(details.arch, process_memory::Architecture::Arch64Bit) {
        return Err("watch only supports 64-bit games so far".to_string());
    }
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
            .ok_or(format!("Unknown address type {}. Known: {:?}", name, all.keys().collect::<Vec<_>>()))?;
        let offsets = &details.address_offsets[&address_type];
        let mut with_base = offsets.clone();
        with_base[0] += base_addr;
        let address = DataMember::<u8>::new_offset(handle, with_base)
            .get_offset()
            .map_err(|e| format!("{:?} {} doesn't resolve: {}", address_type, format_offsets(offsets), e))?;
        watched.push((address_type, address));
    }
    if watched.is_empty() || watched.len() > MAX_WATCHES {
        return Err(format!("Give between 1 and {} address types to watch", MAX_WATCHES));
    }
    let addresses: Vec<usize> = watched.iter().map(|(_, a)| *a).collect();

    let image_size = find_process::get_image_size(handle, base_addr).map_err(|e| e.to_string())?;
    let hits = record(pid, handle, base_addr, &addresses, seconds, write_only, Some((base_addr, base_addr + image_size)))?;

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
