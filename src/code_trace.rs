//! Follows how the game's code uses a static address: which offsets it dereferences from the
//! pointer stored there, and from the pointers loaded from those, and so on. Each memory access is
//! reported as a chain in the same form as the offsets file's (`[static, o1, ..., on]`), so the
//! chains the code itself uses can be found, and matched up between game versions.
//!
//! `Tracer` follows pointer loads, branches, direct calls (arguments and the returned `rax`) and, with a live
//! `CallResolver`, virtual calls; on x86 also into the callers of a getter returning the static's pointer.

use crate::pe_image::PeImage;
use iced_x86::{
    ConstantOffsets, Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind,
    Register,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::OnceLock;

// Limits per trace, so a trace stays roughly within the function it started in and the few it calls
const MAX_INSTRUCTIONS: usize = 600;
const MAX_CALL_DEPTH: usize = 2;
// On x86, a getter (a function returning a pointer it loads from a static) starts at most this far before the
// reference, and at most this many of its callers are followed, each this far
const MAX_GETTER_SIZE: usize = 0x80;
const MAX_GETTER_CALLERS: usize = 64;
const GETTER_CALLER_INSTRUCTIONS: usize = 100;

/// A memory access made through a pointer chain that starts at a static address
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Access {
    pub chain: Vec<i64>,
    pub mnemonic: Mnemonic,
    pub size: usize,
    // The access loads a pointer-sized value into a register that is then followed
    pub loads_pointer: bool,
    // RVA of the instruction making the access
    pub instruction: usize,
}

impl Access {
    /// What has to match for two accesses in different builds to be considered the same access
    pub fn shape(&self) -> (usize, Mnemonic, usize, bool) {
        (self.chain.len(), self.mnemonic, self.size, self.loads_pointer)
    }
}

/// What a register is known to hold
#[derive(Debug, Clone, PartialEq)]
struct Tracked {
    // For an address: `[static]`, the register holds module base + static + add.
    // Otherwise the register holds the pointer read by following `path` (i.e. the address
    // `path ++ [add]` in table form).
    path: Vec<i64>,
    add: i64,
    is_address: bool,
}

impl Tracked {
    fn chain_at(&self, displacement: i64) -> Vec<i64> {
        if self.is_address {
            vec![self.path[0] + self.add + displacement]
        } else {
            let mut chain = self.path.clone();
            chain.push(self.add + displacement);
            chain
        }
    }
}

type State = [Option<Tracked>; 16];

fn slot(register: Register) -> Option<usize> {
    let full = register.full_register();
    if full >= Register::RAX && full <= Register::R15 {
        Some(full as usize - Register::RAX as usize)
    } else {
        None
    }
}

fn has_tracked(state: &State) -> bool {
    state.iter().any(Option::is_some)
}

/// Looks up the RVA of the function a virtual call goes to, given the chain of the call's memory
/// operand (e.g. `[global, 0x0, 0x70]` for `mov rcx, [global]; mov rax, [rcx]; call [rax+0x70]`).
/// Only possible with the game running, since the vtable comes from the object.
pub type CallResolver<'a> = &'a (dyn Fn(&[i64]) -> Option<usize> + Sync);

pub struct Tracer<'a> {
    image: &'a PeImage,
    bitness: u32,
    pointer_size: usize,
    resolver: Option<CallResolver<'a>>,
    // Where each function is called from directly (the return addresses), made when first needed
    callers: OnceLock<HashMap<usize, Vec<usize>>>,
}

struct Walk {
    budget: usize,
    accesses: Vec<Access>,
    info: InstructionInfoFactory,
}

impl<'a> Tracer<'a> {
    pub fn new(image: &'a PeImage) -> Tracer<'a> {
        Tracer {
            image,
            bitness: image.bitness(),
            pointer_size: if image.is_64 { 8 } else { 4 },
            resolver: None,
            callers: OnceLock::new(),
        }
    }

    /// The return addresses of the direct calls (`call rel32`) to each function in the code
    fn callers(&self) -> &HashMap<usize, Vec<usize>> {
        self.callers.get_or_init(|| {
            let data = &self.image.data;
            let mut code: Vec<(usize, usize)> =
                self.image.sections().iter().filter(|s| s.executable).map(|s| (s.start, s.end.min(data.len()))).collect();
            if code.is_empty() {
                code.push((0x1000, data.len()));
            }
            let in_code = |rva: usize| code.iter().any(|(start, end)| (*start..*end).contains(&rva));
            let calls: Vec<(usize, usize)> = code
                .par_iter()
                .flat_map(|(start, end)| (*start..end.saturating_sub(5)).into_par_iter())
                .filter(|&rva| data[rva] == 0xE8)
                .filter_map(|rva| {
                    let displacement = i32::from_le_bytes(data[rva + 1..rva + 5].try_into().unwrap());
                    let target = (rva as i64 + 5 + displacement as i64) as usize;
                    in_code(target).then(|| (target, rva + 5))
                })
                .collect();
            let mut callers: HashMap<usize, Vec<usize>> = HashMap::new();
            for (target, returns_to) in calls {
                callers.entry(target).or_default().push(returns_to);
            }
            callers
        })
    }

    /// The return addresses of the calls to the small function containing `rva`, if it's in one
    fn getter_callers(&self, rva: usize) -> Option<&Vec<usize>> {
        let callers = self.callers();
        (rva.saturating_sub(MAX_GETTER_SIZE)..=rva).rev().find_map(|start| callers.get(&start))
    }

    /// Follow virtual calls too, using the running game to find where they go
    pub fn with_resolver(mut self, resolver: CallResolver<'a>) -> Tracer<'a> {
        self.resolver = Some(resolver);
        self
    }

    fn decode_at(&self, rva: usize) -> Option<Instruction> {
        self.decode_with_offsets(rva).map(|(instruction, _)| instruction)
    }

    fn decode_with_offsets(&self, rva: usize) -> Option<(Instruction, ConstantOffsets)> {
        let data = self.image.data.get(rva..)?;
        let mut decoder = Decoder::with_ip(
            self.bitness,
            &data[..data.len().min(16)],
            self.image.image_base + rva as u64,
            DecoderOptions::NONE,
        );
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            None
        } else {
            let offsets = decoder.get_constant_offsets(&instruction);
            Some((instruction, offsets))
        }
    }

    fn to_rva(&self, va: u64) -> Option<usize> {
        let rva = va.wrapping_sub(self.image.image_base) as usize;
        if rva >= 0x1000 && rva < self.image.data.len() {
            Some(rva)
        } else {
            None
        }
    }

    /// The static address an instruction's memory operand refers to directly, if any
    fn static_operand(&self, instruction: &Instruction) -> Option<usize> {
        if !(0..instruction.op_count()).any(|i| instruction.op_kind(i) == OpKind::Memory) {
            return None;
        }
        match instruction.memory_base() {
            Register::RIP | Register::EIP => self.to_rva(instruction.memory_displacement64()),
            Register::None if instruction.memory_index() == Register::None && !self.image.is_64 => {
                self.to_rva(instruction.memory_displacement64())
            }
            _ => None,
        }
    }

    // 32-bit code often materialises a static address as an immediate (`mov ecx, offset g`)
    fn static_immediate(&self, instruction: &Instruction) -> Option<usize> {
        if self.image.is_64 || instruction.mnemonic() != Mnemonic::Mov || instruction.op1_kind() != OpKind::Immediate32
        {
            return None;
        }
        self.to_rva(instruction.immediate32() as u64)
    }

    /// Finds the instruction containing the 4-byte address field at `position` that refers to `target`.
    /// The earliest start wins: starting later can still decode, but without prefixes (e.g. dropping
    /// REX.W turns `mov rcx, [rip+x]` into `mov ecx, [rip+x]`).
    pub fn instruction_referencing(&self, position: usize, target: usize) -> Option<usize> {
        (position.saturating_sub(10)..position).find(|&start| {
            self.decode_with_offsets(start).map_or(false, |(instruction, offsets)| {
                let at = position - start;
                (offsets.has_displacement()
                    && offsets.displacement_offset() as usize == at
                    && self.static_operand(&instruction) == Some(target))
                    || (offsets.has_immediate()
                        && offsets.immediate_offset() as usize == at
                        && self.static_immediate(&instruction) == Some(target))
            })
        })
    }

    /// Traces the code starting at the instruction at `start`, which should reference a static address
    pub fn trace(&self, start: usize) -> Vec<Access> {
        let mut walk = Walk {
            budget: MAX_INSTRUCTIONS,
            accesses: vec![],
            info: InstructionInfoFactory::new(),
        };
        let returned = self.walk(&mut walk, start, Default::default(), 0, true);
        // A getter's callers use what it returns. TR2013's later builds stopped inlining them; the 64-bit games inline
        // them, and following them there just makes derive slow.
        if let Some(value) = returned.filter(|v| v.path.len() == 1 && !v.is_address && !self.image.is_64) {
            let rax = slot(Register::RAX).unwrap();
            for &returns_to in self.getter_callers(start).into_iter().flatten().take(MAX_GETTER_CALLERS) {
                let mut state: State = Default::default();
                state[rax] = Some(value.clone());
                walk.budget = GETTER_CALLER_INSTRUCTIONS;
                self.walk(&mut walk, returns_to, state, 0, false);
            }
        }
        walk.accesses
    }

    /// Walks the code from `start`, returning what `rax` holds on return if every return agrees
    fn walk(&self, walk: &mut Walk, start: usize, state: State, depth: usize, is_start: bool) -> Option<Tracked> {
        let rax = slot(Register::RAX).unwrap();
        let mut returned: Option<Option<Tracked>> = None;
        let mut visited: HashSet<usize> = HashSet::new();
        let mut pending: Vec<(usize, State, bool)> = vec![(start, state, is_start)];

        while let Some((mut rva, mut state, mut first)) = pending.pop() {
            loop {
                if walk.budget == 0 || !visited.insert(rva) {
                    break;
                }
                walk.budget -= 1;
                let instruction = match self.decode_at(rva) {
                    Some(i) => i,
                    None => break,
                };
                // Where a virtual call goes has to be looked up before the call clobbers anything
                let virtual_target = match (instruction.flow_control(), self.resolver) {
                    (FlowControl::IndirectCall, Some(resolver)) => self
                        .memory_chain(&instruction, &state)
                        .filter(|chain| chain.len() > 1)
                        .and_then(|chain| resolver(&chain)),
                    _ => None,
                };
                self.step(walk, &instruction, &mut state, first, rva);
                first = false;

                let next = rva + instruction.len();
                match instruction.flow_control() {
                    FlowControl::Next => {}
                    FlowControl::UnconditionalBranch => match self.to_rva(instruction.near_branch_target()) {
                        Some(target) if instruction.op0_kind() != OpKind::FarBranch16 => {
                            rva = target;
                            continue;
                        }
                        _ => break,
                    },
                    FlowControl::ConditionalBranch => {
                        if let Some(target) = self.to_rva(instruction.near_branch_target()) {
                            pending.push((target, state.clone(), false));
                        }
                    }
                    FlowControl::Call => {
                        let result = self
                            .to_rva(instruction.near_branch_target())
                            .and_then(|target| self.call(walk, target, &state, depth));
                        self.clobber_volatile(&mut state);
                        state[rax] = result;
                    }
                    FlowControl::IndirectCall => {
                        let result = virtual_target.and_then(|target| self.call(walk, target, &state, depth));
                        self.clobber_volatile(&mut state);
                        state[rax] = result;
                    }
                    FlowControl::Return => {
                        returned = match returned {
                            None => Some(state[rax].clone()),
                            Some(previous) if previous == state[rax] => Some(previous),
                            Some(_) => Some(None),
                        };
                        break;
                    }
                    _ => break,
                }
                if !has_tracked(&state) {
                    break;
                }
                rva = next;
            }
        }
        returned.flatten()
    }

    fn call(&self, walk: &mut Walk, target: usize, state: &State, depth: usize) -> Option<Tracked> {
        if depth >= MAX_CALL_DEPTH {
            return None;
        }
        // Arguments that carry a tracked pointer: rcx/rdx/r8/r9 on x64, `this` in ecx on x86
        let arguments: &[Register] = if self.image.is_64 {
            &[Register::RCX, Register::RDX, Register::R8, Register::R9]
        } else {
            &[Register::RCX]
        };
        let mut callee: State = Default::default();
        for register in arguments {
            let s = slot(*register).unwrap();
            callee[s] = state[s].clone();
        }
        if has_tracked(&callee) {
            self.walk(walk, target, callee, depth + 1, false)
        } else {
            None
        }
    }

    fn clobber_volatile(&self, state: &mut State) {
        let volatile: &[Register] = if self.image.is_64 {
            &[
                Register::RAX,
                Register::RCX,
                Register::RDX,
                Register::R8,
                Register::R9,
                Register::R10,
                Register::R11,
            ]
        } else {
            &[Register::RAX, Register::RCX, Register::RDX]
        };
        for register in volatile {
            state[slot(*register).unwrap()] = None;
        }
    }

    /// An instruction's displacement, sign extended (iced zero extends 32-bit addressing's)
    fn displacement(&self, instruction: &Instruction) -> i64 {
        if self.image.is_64 {
            instruction.memory_displacement64() as i64
        } else {
            instruction.memory_displacement32() as i32 as i64
        }
    }

    /// The chain an instruction's memory operand refers to, if it's static or based on a tracked register
    fn memory_chain(&self, instruction: &Instruction, state: &State) -> Option<Vec<i64>> {
        if !(0..instruction.op_count()).any(|i| instruction.op_kind(i) == OpKind::Memory) {
            None
        } else if let Some(address) = self.static_operand(instruction) {
            Some(vec![address as i64])
        } else if instruction.memory_index() == Register::None {
            slot(instruction.memory_base())
                .and_then(|s| state[s].as_ref())
                .map(|t| t.chain_at(self.displacement(instruction)))
        } else {
            None
        }
    }

    fn step(&self, walk: &mut Walk, instruction: &Instruction, state: &mut State, is_start: bool, rva: usize) {
        let static_address = self.static_operand(instruction);
        let memory_chain = self.memory_chain(instruction, state);

        let destination = if instruction.op_count() > 0 && instruction.op0_kind() == OpKind::Register {
            Some(instruction.op0_register())
        } else {
            None
        };
        let destination_is_pointer =
            destination.map_or(false, |r| r.size() == self.pointer_size && slot(r).is_some());

        // Work out the new value of the destination before clobbering anything
        let mut new_value: Option<Tracked> = None;
        let mut loads_pointer = false;
        match instruction.mnemonic() {
            Mnemonic::Mov if destination_is_pointer && instruction.op1_kind() == OpKind::Memory => {
                if let Some(chain) = &memory_chain {
                    new_value = Some(Tracked {
                        path: chain.clone(),
                        add: 0,
                        is_address: false,
                    });
                    loads_pointer = true;
                }
            }
            Mnemonic::Mov if destination_is_pointer && instruction.op1_kind() == OpKind::Register => {
                new_value = slot(instruction.op1_register()).and_then(|s| state[s].clone());
            }
            Mnemonic::Mov if destination_is_pointer => {
                new_value = self.static_immediate(instruction).map(|address| Tracked {
                    path: vec![address as i64],
                    add: 0,
                    is_address: true,
                });
            }
            Mnemonic::Lea if destination_is_pointer => {
                if let Some(address) = static_address {
                    new_value = Some(Tracked {
                        path: vec![address as i64],
                        add: 0,
                        is_address: true,
                    });
                } else if instruction.memory_index() == Register::None {
                    new_value = slot(instruction.memory_base())
                        .and_then(|s| state[s].clone())
                        .map(|mut t| {
                            t.add += self.displacement(instruction);
                            t
                        });
                }
            }
            Mnemonic::Add | Mnemonic::Sub if destination_is_pointer => {
                let immediate = match instruction.op1_kind() {
                    OpKind::Immediate8to64 | OpKind::Immediate32to64 => Some(instruction.immediate64() as i64),
                    OpKind::Immediate8to32 | OpKind::Immediate32 => Some(instruction.immediate32() as i32 as i64),
                    _ => None,
                };
                if let (Some(immediate), Some(s)) = (immediate, slot(destination.unwrap())) {
                    new_value = state[s].clone().map(|mut t| {
                        t.add += if instruction.mnemonic() == Mnemonic::Add { immediate } else { -immediate };
                        t
                    });
                }
            }
            _ => {}
        }

        // Only record static accesses at the start of a trace; every static access starts its own trace
        if let Some(chain) = memory_chain {
            if static_address.is_none() || is_start {
                if instruction.mnemonic() != Mnemonic::Lea && instruction.mnemonic() != Mnemonic::Nop {
                    walk.accesses.push(Access {
                        chain,
                        mnemonic: instruction.mnemonic(),
                        size: instruction.memory_size().size(),
                        loads_pointer,
                        instruction: rva,
                    });
                }
            }
        }

        // Anything this instruction writes no longer holds a tracked value
        let info = walk.info.info(instruction);
        for used in info.used_registers() {
            match used.access() {
                OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite => {
                    if let Some(s) = slot(used.register()) {
                        state[s] = None;
                    }
                }
                _ => {}
            }
        }
        if let (Some(value), Some(s)) = (new_value, destination.and_then(slot)) {
            state[s] = Some(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(code: &[u8], is_64: bool) -> PeImage {
        let mut data = vec![0xCC_u8; 0x3000];
        data[0x1000..0x1000 + code.len()].copy_from_slice(code);
        PeImage {
            data,
            is_64,
            image_base: if is_64 { 0x1_4000_0000 } else { 0x40_0000 },
        }
    }

    #[test]
    fn follows_pointer_chain_from_rip_relative_global() {
        let code = [
            0x48, 0x8B, 0x05, 0xF9, 0x0F, 0x00, 0x00, // mov rax, [rip+0xFF9] -> 0x2000
            0x48, 0x8B, 0x88, 0x60, 0x0C, 0x00, 0x00, // mov rcx, [rax+0xC60]
            0xF3, 0x0F, 0x10, 0x41, 0x60, // movss xmm0, [rcx+0x60]
            0xC3, // ret
        ];
        let image = image(&code, true);
        let accesses = Tracer::new(&image).trace(0x1000);
        let chains: Vec<Vec<i64>> = accesses.iter().map(|a| a.chain.clone()).collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, 0xC60], vec![0x2000, 0xC60, 0x60]]);
        assert!(accesses[1].loads_pointer);
        assert!(!accesses[2].loads_pointer);
    }

    #[test]
    fn follows_this_pointer_into_calls() {
        let code = [
            0x48, 0x8B, 0x0D, 0xF9, 0x0F, 0x00, 0x00, // mov rcx, [rip+0xFF9] -> 0x2000
            0xE8, 0x01, 0x00, 0x00, 0x00, // call +1
            0xC3, // ret
            0x8B, 0x41, 0x10, // callee: mov eax, [rcx+0x10]
            0xC3,
        ];
        let image = image(&code, true);
        let chains: Vec<Vec<i64>> = Tracer::new(&image).trace(0x1000).into_iter().map(|a| a.chain).collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, 0x10]]);
    }

    #[test]
    fn finds_the_start_including_prefixes() {
        let code = [0x90, 0x48, 0x8B, 0x0D, 0xF8, 0x0F, 0x00, 0x00, 0xC3]; // nop; mov rcx, [rip+0xFF8] -> 0x2000
        let image = image(&code, true);
        assert_eq!(Tracer::new(&image).instruction_referencing(0x1004, 0x2000), Some(0x1001));
    }

    #[test]
    fn follows_pointers_returned_by_getters() {
        let code = [
            0x48, 0x8B, 0x0D, 0xF9, 0x0F, 0x00, 0x00, // mov rcx, [rip+0xFF9] -> 0x2000
            0xE8, 0x05, 0x00, 0x00, 0x00, // call getter
            0x0F, 0x28, 0x40, 0x30, // movaps xmm0, [rax+0x30]
            0xC3, // ret
            0x48, 0x8B, 0x41, 0x08, // getter: mov rax, [rcx+8]
            0xC3,
        ];
        let image = image(&code, true);
        let chains: Vec<Vec<i64>> = Tracer::new(&image).trace(0x1000).into_iter().map(|a| a.chain).collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, 0x8], vec![0x2000, 0x8, 0x30]]);
    }

    #[test]
    fn follows_virtual_calls_with_a_resolver() {
        let code = [
            0x48, 0x8B, 0x0D, 0xF9, 0x0F, 0x00, 0x00, // mov rcx, [rip+0xFF9] -> 0x2000
            0x48, 0x8B, 0x01, // mov rax, [rcx]
            0xFF, 0x50, 0x70, // call [rax+0x70]
            0xC3, // ret
            0x83, 0x79, 0x10, 0x02, // method at 0x100E: cmp dword [rcx+0x10], 2
            0xC3,
        ];
        let image = image(&code, true);
        let resolver = |chain: &[i64]| if chain == [0x2000, 0x0, 0x70] { Some(0x100E) } else { None };
        let chains: Vec<Vec<i64>> = Tracer::new(&image)
            .with_resolver(&resolver)
            .trace(0x1000)
            .into_iter()
            .map(|a| a.chain)
            .collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, 0x0], vec![0x2000, 0x0, 0x70], vec![0x2000, 0x10]]);
    }

    #[test]
    fn follows_getters_into_their_callers() {
        let code = [
            0xE8, 0x05, 0x00, 0x00, 0x00, // call getter
            0x8B, 0x48, 0x50, // mov ecx, [eax+0x50]
            0xC3, // ret
            0xCC, // padding
            0xA1, 0x00, 0x20, 0x40, 0x00, // getter at 0x100A: mov eax, [0x402000]
            0xC3,
        ];
        let image = image(&code, false);
        let tracer = Tracer::new(&image);
        let chains: Vec<Vec<i64>> = tracer.trace(0x100A).into_iter().map(|a| a.chain).collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, 0x50]]);
    }

    #[test]
    fn negative_offsets_on_x86_stay_negative() {
        let code = [
            0x8B, 0x0D, 0x00, 0x20, 0x40, 0x00, // mov ecx, [0x402000]
            0x8D, 0x41, 0xF0, // lea eax, [ecx-0x10]
            0x8B, 0x50, 0x04, // mov edx, [eax+4]
            0xC3,
        ];
        let image = image(&code, false);
        let chains: Vec<Vec<i64>> = Tracer::new(&image).trace(0x1000).into_iter().map(|a| a.chain).collect();
        assert_eq!(chains, vec![vec![0x2000], vec![0x2000, -0xC]]);
    }

    #[test]
    fn handles_absolute_struct_globals_on_x86() {
        let code = [
            0xB9, 0x00, 0x20, 0x40, 0x00, // mov ecx, 0x402000
            0xD9, 0x81, 0x20, 0x02, 0x00, 0x00, // fld dword [ecx+0x220]
            0xC3,
        ];
        let image = image(&code, false);
        let chains: Vec<Vec<i64>> = Tracer::new(&image).trace(0x1000).into_iter().map(|a| a.chain).collect();
        assert_eq!(chains, vec![vec![0x2220]]);
    }
}
