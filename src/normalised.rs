//! Matches references to statics between builds made with different compilers (e.g. TR2013's original release and
//! its later Steam build), where the bytes around a reference have little in common: registers, stack frames,
//! instruction choice and every address change. Instead the code around each reference is described by what survives
//! a recompile: the strings and imports it uses, its constants and field offsets, and the kinds of instructions it
//! has. References are matched by how much of that they share, each part weighted by how rare it is.

use crate::derive::References;
use crate::pe_image::{PeImage, Section};
use iced_x86::{CodeSize, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register};
use rayon::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

// Instructions either side of a reference that describe it
const CONTEXT: usize = 12;
// How far back to start decoding to find the instructions before a reference
const LOOK_BACK: usize = 80;
// Parts of a description seen at more than this many references in the new build are too common to find
// candidates by
const MAX_POSTINGS: usize = 2000;
// Candidates are found by the rarest parts of a description first, and more common ones are added while there are
// fewer than this many
const CANDIDATES: usize = 2000;
// Score (out of 1) the best match must reach, and how far it must beat the runner up by
pub const MIN_SCORE: f64 = 0.45;
pub const MIN_MARGIN: f64 = 0.1;
pub const MIN_PAIR_SCORE: f64 = 0.35;
// Longest string described by its contents
const MAX_STRING: usize = 128;
// References this close to the best match are its neighbours, not rivals to it
const NEARBY: usize = 64;

fn hash(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn is_branch(kind: OpKind) -> bool {
    matches!(kind, OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64)
}

/// The start of the instruction whose 4-byte address field (displacement or immediate) is at `position`
pub fn instruction_at(image: &PeImage, position: usize) -> Option<usize> {
    let bitness = if image.is_64 { 64 } else { 32 };
    (position.saturating_sub(10)..position).find(|&start| {
        let data = &image.data[start..image.data.len().min(start + 16)];
        let mut decoder = Decoder::with_ip(bitness, data, image.image_base + start as u64, DecoderOptions::NONE);
        let instruction = decoder.decode();
        // A relative branch's displacement can look like an address on x86, but isn't one
        if instruction.is_invalid() || (0..instruction.op_count()).any(|i| is_branch(instruction.op_kind(i))) {
            return false;
        }
        let offsets = decoder.get_constant_offsets(&instruction);
        let at = position - start;
        (offsets.has_displacement() && offsets.displacement_offset() == at && offsets.displacement_size() == 4)
            || (offsets.has_immediate() && offsets.immediate_offset() == at && offsets.immediate_size() == 4)
    })
}

/// Describes code in one image
struct Describer<'a> {
    image: &'a PeImage,
    sections: Vec<Section>,
    imports: HashMap<usize, String>,
    bitness: u32,
}

impl<'a> Describer<'a> {
    fn new(image: &'a PeImage) -> Describer<'a> {
        Describer {
            image,
            sections: image.sections(),
            imports: image.imports(),
            bitness: if image.is_64 { 64 } else { 32 },
        }
    }

    fn section(&self, rva: usize) -> Option<&Section> {
        self.sections.iter().find(|s| s.start <= rva && rva < s.end)
    }

    fn is_code(&self, rva: usize) -> bool {
        self.section(rva).map_or(false, |s| s.executable)
    }

    fn decode(&self, rva: usize) -> Option<Instruction> {
        let data = self.image.data.get(rva..)?;
        let mut decoder =
            Decoder::with_ip(self.bitness, &data[..data.len().min(16)], self.image.image_base + rva as u64, DecoderOptions::NONE);
        let instruction = decoder.decode();
        (!instruction.is_invalid()).then(|| instruction)
    }

    /// The instructions before the one at `start`, nearest last, stopping at the start of the function
    fn before(&self, start: usize) -> Vec<Instruction> {
        let from = start.saturating_sub(LOOK_BACK);
        // x86 decoding falls back into step quickly, so try a few starting points until one lands on `start`
        for skew in 0..16 {
            let mut rva = from + skew;
            let mut instructions = vec![];
            while rva < start {
                match self.decode(rva) {
                    Some(instruction) => {
                        rva += instruction.len();
                        instructions.push(instruction);
                    }
                    None => break,
                }
            }
            if rva == start {
                let function_start = instructions
                    .iter()
                    .rposition(|i| i.flow_control() == FlowControl::Return || i.mnemonic() == Mnemonic::Int3)
                    .map_or(0, |i| i + 1);
                let instructions = &instructions[function_start..];
                return instructions[instructions.len().saturating_sub(CONTEXT)..].to_vec();
            }
        }
        vec![]
    }

    /// The instruction at `start` and those after it, stopping at the end of the function
    fn after(&self, start: usize) -> Vec<Instruction> {
        let mut instructions = vec![];
        let mut rva = start;
        while instructions.len() <= CONTEXT {
            let instruction = match self.decode(rva) {
                Some(i) if i.mnemonic() != Mnemonic::Int3 => i,
                _ => break,
            };
            rva += instruction.len();
            instructions.push(instruction);
            if instruction.flow_control() == FlowControl::Return {
                break;
            }
        }
        instructions
    }

    fn string_at(&self, rva: usize) -> Option<String> {
        let data = self.image.data.get(rva..)?;
        let printable = |b: u8| (0x20..0x7F).contains(&b) || b == b'\t' || b == b'\n' || b == b'\r';
        let ascii = data.iter().take(MAX_STRING).position(|b| !printable(*b));
        if let Some(end) = ascii.filter(|&end| end >= 4 && data[end] == 0) {
            return Some(String::from_utf8_lossy(&data[..end]).into_owned());
        }
        let wide: Vec<u16> = data
            .chunks_exact(2)
            .take(MAX_STRING)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|c| *c < 0x80 && printable(*c as u8))
            .collect();
        let terminated = data.get(wide.len() * 2..wide.len() * 2 + 2) == Some(&[0, 0]);
        (wide.len() >= 4 && terminated).then(|| String::from_utf16_lossy(&wide))
    }

    /// What's at a static address: the import, string or constant there by name or value, or otherwise just what
    /// kind of section it's in, since the address itself changes between builds
    fn describe_address(&self, address: u64, size: usize, hasher: &mut DefaultHasher) {
        let rva = address.wrapping_sub(self.image.image_base) as usize;
        let section = match self.section(rva) {
            Some(s) => *s,
            None => {
                "not an address".hash(hasher);
                address.hash(hasher);
                return;
            }
        };
        if let Some(name) = self.imports.get(&rva) {
            ("import", name).hash(hasher);
        } else if section.executable {
            "code".hash(hasher);
        // Writable data is only filled in at run time, so it's different in a dump of a running game
        } else if let Some(string) = self.string_at(rva).filter(|_| !section.writable) {
            ("string", string).hash(hasher);
        } else if !section.writable && (1..=16).contains(&size) {
            ("constant", self.image.data.get(rva..rva + size)).hash(hasher);
        } else {
            ("data", section.writable).hash(hasher);
        }
    }

    fn is_address(&self, value: u64) -> bool {
        !self.image.is_64 && self.section(value.wrapping_sub(self.image.image_base) as usize).is_some()
    }

    /// One instruction, without anything that a recompile changes for no reason
    fn token(&self, instruction: &Instruction) -> u64 {
        let mut hasher = DefaultHasher::new();
        // Branches get inverted and reordered, so they're all alike
        if instruction.is_jcc_short_or_near() {
            "jcc".hash(&mut hasher);
            return hasher.finish();
        }
        (instruction.mnemonic() as u16).hash(&mut hasher);
        for i in 0..instruction.op_count() {
            match instruction.op_kind(i) {
                OpKind::Register => {
                    let register = instruction.op_register(i);
                    (register.size(), register.is_xmm(), register.is_st()).hash(&mut hasher);
                }
                kind if is_branch(kind) => "branch".hash(&mut hasher),
                OpKind::Memory => self.memory(instruction, &mut hasher),
                OpKind::Immediate8
                | OpKind::Immediate8_2nd
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate64
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
                | OpKind::Immediate8to64
                | OpKind::Immediate32to64 => {
                    let value = instruction.immediate(i);
                    if self.is_address(value) {
                        self.describe_address(value, 0, &mut hasher);
                    } else if (value as i64).unsigned_abs() < 0x10000 || (value as i32).unsigned_abs() < 0x10000 {
                        ("immediate", value as u32).hash(&mut hasher);
                    } else {
                        "large immediate".hash(&mut hasher);
                    }
                }
                kind => (kind as u8).hash(&mut hasher),
            }
        }
        hasher.finish()
    }

    fn memory(&self, instruction: &Instruction, hasher: &mut DefaultHasher) {
        let size = instruction.memory_size().size();
        size.hash(hasher);
        let base = instruction.memory_base();
        let index = instruction.memory_index();
        let displacement = if instruction.code_size() == CodeSize::Code64 {
            instruction.memory_displacement64() as i64
        } else {
            instruction.memory_displacement32() as i32 as i64
        };
        if base == Register::RIP || base == Register::EIP || (base == Register::None && !self.image.is_64) {
            (index != Register::None).hash(hasher);
            self.describe_address(instruction.memory_displacement64(), size, hasher);
        } else if matches!(base.full_register(), Register::RSP | Register::RBP) {
            // Locals and arguments move with the stack frame
            "stack".hash(hasher);
        } else {
            ("field", displacement, index != Register::None, instruction.memory_index_scale()).hash(hasher);
        }
    }

    /// The description of the code around the reference whose address field is at `position`: a sorted multiset
    /// of instruction tokens, plus one for the referencing instruction itself, which matters most
    fn describe(&self, position: usize) -> Option<Vec<u64>> {
        if !self.is_code(position) {
            return None;
        }
        let start = instruction_at(self.image, position)?;
        let after = self.after(start);
        let site = self.token(after.first()?);
        let mut tokens: Vec<u64> = self.before(start).iter().chain(&after).map(|i| self.token(i)).collect();
        tokens.push(hash(("site", site)));
        tokens.sort_unstable();
        Some(tokens)
    }
}

/// The new build's references, described, with an index of where each rare part of a description is found
pub struct Matcher<'a> {
    old: Describer<'a>,
    descriptions: HashMap<usize, Vec<u64>>,
    // The static each new reference refers to
    targets: HashMap<usize, u32>,
    postings: HashMap<u64, Vec<usize>>,
    weights: HashMap<u64, f64>,
}

impl<'a> Matcher<'a> {
    pub fn new(old: &References<'a>, new: &References<'a>) -> Matcher<'a> {
        let describer = Describer::new(new.image);
        let mut positions: Vec<usize> = new.all().iter().map(|(_, p)| *p as usize).collect();
        positions.sort_unstable();
        positions.dedup();
        let descriptions: HashMap<usize, Vec<u64>> = positions
            .par_iter()
            .filter_map(|&p| describer.describe(p).map(|d| (p, d)))
            .collect();

        let mut counts: HashMap<u64, usize> = HashMap::new();
        for tokens in descriptions.values() {
            let unique: HashSet<&u64> = tokens.iter().collect();
            for token in unique {
                *counts.entry(*token).or_default() += 1;
            }
        }
        let total = descriptions.len() as f64;
        let weights: HashMap<u64, f64> = counts.iter().map(|(t, c)| (*t, (total / *c as f64).ln())).collect();
        let mut postings: HashMap<u64, Vec<usize>> = HashMap::new();
        for (&position, tokens) in &descriptions {
            for token in tokens {
                if counts[token] <= MAX_POSTINGS {
                    postings.entry(*token).or_default().push(position);
                }
            }
        }
        for list in postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        Matcher {
            old: Describer::new(old.image),
            descriptions,
            targets: new.all().iter().map(|(t, p)| (*p as usize, *t)).collect(),
            postings,
            weights,
        }
    }

    fn norm(&self, tokens: &[u64]) -> f64 {
        tokens.iter().map(|t| self.weights.get(t).map_or(0.0, |w| w * w)).sum::<f64>().sqrt()
    }

    /// Weighted cosine similarity of two descriptions, from 0 to 1
    fn score(&self, a: &[u64], b: &[u64]) -> f64 {
        let (mut i, mut j, mut shared) = (0, 0, 0.0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                shared += self.weights.get(&a[i]).map_or(0.0, |w| w * w);
                i += 1;
                j += 1;
            } else if a[i] < b[j] {
                i += 1;
            } else {
                j += 1;
            }
        }
        let norms = self.norm(a) * self.norm(b);
        if norms == 0.0 {
            0.0
        } else {
            shared / norms
        }
    }

    /// The best unambiguous match in the new build for the reference at `position` in the old one, with its score
    pub fn best_match(&self, position: usize) -> Option<(usize, f64)> {
        let tokens = self.old.describe(position)?;
        let mut rarest: Vec<&Vec<usize>> = tokens.iter().filter_map(|t| self.postings.get(t)).collect();
        rarest.sort_by_key(|list| list.len());
        rarest.dedup();
        let mut candidates: Vec<usize> = vec![];
        for list in rarest {
            if !candidates.is_empty() && candidates.len() + list.len() > CANDIDATES {
                break;
            }
            candidates.extend(list);
        }
        candidates.sort_unstable();
        candidates.dedup();
        let scores: Vec<(f64, usize)> = candidates.iter().map(|q| (self.score(&tokens, &self.descriptions[q]), *q)).collect();
        let best = scores.iter().copied().fold((0.0, 0), |best, s| if s.0 > best.0 { s } else { best });
        // Rivals are references to other statics, elsewhere: one static's references often look alike
        let second = scores
            .iter()
            .filter(|(_, q)| q.abs_diff(best.1) > NEARBY && self.targets.get(q) != self.targets.get(&best.1))
            .map(|(score, _)| *score)
            .fold(0.0, f64::max);
        (best.0 >= MIN_SCORE && best.0 - second >= MIN_MARGIN).then(|| (best.1, best.0))
    }

    /// The new reference most like the old one at `old_position`, and how alike they are (from 0 to 1)
    pub fn most_similar(&self, old_position: usize, candidates: impl Iterator<Item = usize>) -> Option<(usize, f64)> {
        let tokens = self.old.describe(old_position)?;
        candidates
            .filter_map(|q| self.descriptions.get(&q).map(|d| (q, self.score(&tokens, d))))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(code: &[u8]) -> PeImage {
        let mut data = vec![0xCC_u8; 0x3000];
        data[0x1000..0x1000 + code.len()].copy_from_slice(code);
        PeImage {
            data,
            is_64: false,
            image_base: 0x40_0000,
        }
    }

    #[test]
    fn describes_code_the_same_regardless_of_registers_and_frames() {
        // mov eax, [0x402000]; mov ecx, [esp+8]; fld dword [eax+0x220]; ret
        let a = image(&[0xA1, 0x00, 0x20, 0x40, 0x00, 0x8B, 0x4C, 0x24, 0x08, 0xD9, 0x80, 0x20, 0x02, 0x00, 0x00, 0xC3]);
        // mov edx, [0x402100]; mov ecx, [ebp+0xC]; fld dword [edx+0x220]; ret
        let b = image(&[0x8B, 0x15, 0x00, 0x21, 0x40, 0x00, 0x8B, 0x4D, 0x0C, 0xD9, 0x82, 0x20, 0x02, 0x00, 0x00, 0xC3]);
        let (a, b) = (Describer::new(&a), Describer::new(&b));
        let tokens = |d: &Describer, start: usize| d.after(start).iter().skip(1).map(|i| d.token(i)).collect::<Vec<_>>();
        assert_eq!(tokens(&a, 0x1000), tokens(&b, 0x1000));
        assert_eq!(instruction_at(a.image, 0x1001), Some(0x1000));
        assert_eq!(instruction_at(b.image, 0x1002), Some(0x1000));
    }
}
