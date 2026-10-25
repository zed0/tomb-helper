//! Matches "witness" instructions (ones known to access a value, found with `tomb-helper watch`)
//! from one build into another, to see how the field offset they use has changed.

use crate::pe_image::PeImage;
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
use rayon::prelude::*;
use std::collections::HashMap;

// Bytes compared either side of an address or displacement field
pub const WINDOW: usize = 32;
// Fraction of window bytes that must match, and how far the best match must beat the runner-up
pub const MIN_SCORE: f64 = 0.6;
pub const MIN_MARGIN: f64 = 0.1;
// How many `key`s there are
pub const KEYS: usize = 1 << 16;
// The longest an x86 instruction can be
const MAX_INSTRUCTION_LENGTH: usize = 15;

/// Where an instruction's displacement is, and its value
#[derive(Debug, Clone, Copy, PartialEq)]
struct Displacement {
    position: usize,
    size: usize,
    value: i64,
}

fn displacement_at(image: &PeImage, start: usize) -> Option<Displacement> {
    let data = image.data.get(start..)?;
    let mut decoder = Decoder::with_ip(
        image.bitness(),
        &data[..data.len().min(MAX_INSTRUCTION_LENGTH)],
        image.image_base + start as u64,
        DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    let offsets = decoder.get_constant_offsets(&instruction);
    if instruction.is_invalid() || !offsets.has_displacement() {
        return None;
    }
    let value = if image.is_64 {
        instruction.memory_displacement64() as i64
    } else {
        instruction.memory_displacement32() as i32 as i64
    };
    Some(Displacement {
        position: start + offsets.displacement_offset(),
        size: offsets.displacement_size(),
        value,
    })
}

/// The two bytes before an address or displacement field (usually opcode + ModRM), below `KEYS`
pub fn key(data: &[u8], position: usize) -> usize {
    (data[position - 2] as usize) << 8 | data[position - 1] as usize
}

/// How many of the `WINDOW` bytes either side of a `size` byte field at `p` in `a` and `q` in `b` are the same
pub fn similarity(a: &[u8], p: usize, b: &[u8], q: usize, size: usize) -> usize {
    let same = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(i, j)| i == j).count();
    same(&a[p - WINDOW..p], &b[q - WINDOW..q]) + same(&a[p + size..p + size + WINDOW], &b[q + size..q + size + WINDOW])
}

/// A witness matched into the new build
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matched {
    pub old_displacement: i64,
    pub new_displacement: i64,
    pub score: f64,
    // RVA of the instruction it matched in the new build
    pub new_instruction: usize,
}

/// Matches each witness instruction (by RVA in `old`) into `new`, returning one result per witness
pub fn match_witnesses(old: &PeImage, new: &PeImage, witnesses: &[usize]) -> Vec<Option<Matched>> {
    let old_displacements: Vec<Option<Displacement>> = witnesses.iter().map(|w| displacement_at(old, *w)).collect();

    // One pass over the new image collects every position with a key we're looking for
    let mut wanted = vec![false; KEYS];
    for d in old_displacements.iter().flatten() {
        wanted[key(&old.data, d.position)] = true;
    }
    let end = new.data.len().saturating_sub(WINDOW + 4);
    let candidates: Vec<u32> = (WINDOW..end)
        .into_par_iter()
        .filter(|q| wanted[key(&new.data, *q)])
        .map(|q| q as u32)
        .collect();
    let mut by_key: HashMap<usize, Vec<usize>> = HashMap::new();
    for q in candidates {
        by_key.entry(key(&new.data, q as usize)).or_default().push(q as usize);
    }

    old_displacements
        .par_iter()
        .map(|d| {
            let d = (*d)?;
            let mut best = (0, 0);
            let mut second = 0;
            for &q in by_key.get(&key(&old.data, d.position))? {
                let score = similarity(&old.data, d.position, &new.data, q, d.size);
                if score > best.0 {
                    second = best.0;
                    best = (score, q);
                } else if score > second {
                    second = score;
                }
            }
            let fraction = |s: usize| s as f64 / (2 * WINDOW) as f64;
            if fraction(best.0) < MIN_SCORE || fraction(best.0 - second) < MIN_MARGIN {
                return None;
            }
            // Find the start of the matched instruction: the one whose displacement is at `best.1`
            let q = best.1;
            let (new_instruction, new_displacement) = (q.saturating_sub(14)..q)
                .filter_map(|start| displacement_at(new, start).map(|n| (start, n)))
                .find(|(_, n)| n.position == q && n.size == d.size)?;
            Some(Matched {
                old_displacement: d.value,
                new_displacement: new_displacement.value,
                score: fraction(best.0),
                new_instruction,
            })
        })
        .collect()
}

// Instructions shown around a witness by `print_matches`, decoded from at most this far before it
const CONTEXT_BEFORE: usize = 24;
const CONTEXT_INSTRUCTIONS: usize = 7;

// The instructions around the one at `rva`, which is marked
fn context(image: &PeImage, rva: usize) -> Vec<String> {
    let ip = image.image_base + rva as u64;
    let decode = |start: usize, length: usize| {
        let data = &image.data[start..image.data.len().min(start + length)];
        Decoder::with_ip(image.bitness(), data, image.image_base + start as u64, DecoderOptions::NONE).into_iter().collect::<Vec<_>>()
    };
    // From the earliest start that decodes into the instruction at `rva`
    let start = (rva.saturating_sub(CONTEXT_BEFORE)..=rva)
        .find(|&start| decode(start, rva - start + 16).iter().any(|i| i.ip() == ip))
        .unwrap_or(rva);
    let instructions = decode(start, 0x80);
    let at = instructions.iter().position(|i| i.ip() == ip).unwrap_or(0);
    let mut formatter = IntelFormatter::new();
    instructions
        .iter()
        .skip(at.saturating_sub(CONTEXT_INSTRUCTIONS / 2))
        .take(CONTEXT_INSTRUCTIONS)
        .map(|i| {
            let mut text = String::new();
            formatter.format(i, &mut text);
            let mark = if i.ip() == ip { ">" } else { " " };
            format!("{} +{:X}  {}", mark, i.ip() - image.image_base, text)
        })
        .collect()
}

// How far before an instruction to look for where its base register was set
const BASE_SEARCH: usize = 0x400;

// Where the base register of the witness at `rva` was last set before it, as an offset from another register (`lea
// r14, [rcx+0x570]`, or `add r14, 0x570` after `mov r14, rcx`): the instruction's RVA and the offset. Else what set it
// some other way (e.g. loading it as a pointer, when the witness says nothing about where the field is in the object
// the chain reaches), or that nothing did as far back as it looks. Only follows the code in a straight line, so a branch
// in between can make it wrong.
fn base_setter(image: &PeImage, rva: usize) -> Result<(usize, i64), String> {
    let found = base_search(image, rva);
    found.ok_or_else(|| {
        let ip = image.image_base + rva as u64;
        let decode = |start: usize| {
            let data = &image.data[start..image.data.len().min(rva + 16)];
            Decoder::with_ip(image.bitness(), data, image.image_base + start as u64, DecoderOptions::NONE).into_iter().collect::<Vec<_>>()
        };
        let instructions = (rva.saturating_sub(BASE_SEARCH)..=rva).map(decode).find(|d| d.iter().any(|i| i.ip() == ip));
        let instructions = match instructions {
            Some(i) => i,
            None => return "it couldn't be decoded".to_string(),
        };
        let at = instructions.iter().position(|i| i.ip() == ip).unwrap_or(0);
        let base = instructions[at].memory_base().full_register();
        let mut formatter = IntelFormatter::new();
        match instructions[..at].iter().rev().find(|i| {
            i.op_count() > 0 && i.op0_kind() == iced_x86::OpKind::Register && i.op0_register().full_register() == base
        }) {
            Some(i) => {
                let mut text = String::new();
                formatter.format(i, &mut text);
                format!("{:?} was set by +{:X} {}", base, i.ip() - image.image_base, text)
            }
            None => format!("{:?} isn't set in the {:#X} bytes before it", base, BASE_SEARCH),
        }
    })
}

fn base_search(image: &PeImage, rva: usize) -> Option<(usize, i64)> {
    let ip = image.image_base + rva as u64;
    let decode = |start: usize| {
        let data = &image.data[start..image.data.len().min(rva + 16)];
        Decoder::with_ip(image.bitness(), data, image.image_base + start as u64, DecoderOptions::NONE).into_iter().collect::<Vec<_>>()
    };
    // The earliest start that decodes into the instruction at `rva` (decoding resynchronises within a few instructions)
    let instructions = (rva.saturating_sub(BASE_SEARCH)..=rva).map(decode).find(|d| d.iter().any(|i| i.ip() == ip))?;
    let at = instructions.iter().position(|i| i.ip() == ip)?;
    let base = instructions[at].memory_base().full_register();
    let mut offset = 0i64;
    for instruction in instructions[..at].iter().rev() {
        if instruction.op_count() == 0 || instruction.op0_kind() != iced_x86::OpKind::Register {
            continue;
        }
        if instruction.op0_register().full_register() != base {
            continue;
        }
        let displacement = if image.is_64 { instruction.memory_displacement64() as i64 } else { instruction.memory_displacement32() as i32 as i64 };
        match instruction.mnemonic() {
            iced_x86::Mnemonic::Lea if instruction.memory_index() == iced_x86::Register::None => {
                return Some((instruction.ip() as usize - image.image_base as usize, offset + displacement));
            }
            iced_x86::Mnemonic::Add if instruction.op1_kind() != iced_x86::OpKind::Register => {
                offset += instruction.immediate(1) as i64;
            }
            // Set some other way, so the offset it was given isn't known
            _ => return None,
        }
    }
    None
}

/// `debug match-witnesses <from> <to> <value or group.field>`: where each of `from`'s witnesses for it matched in `to`,
/// with the code around both
pub fn print_matches(from: &str, to: &str, name: &str, from_version: Option<&str>) -> anyhow::Result<()> {
    use anyhow::Context;
    let (old, new) = (crate::derive::load(from)?, crate::derive::load(to)?);
    let details = crate::derive::reference_details(&old, from_version)?;
    let witnesses = if name.contains('.') {
        details.layout_witnesses.get(name).cloned()
    } else {
        details.witnesses.iter().find(|(t, _)| format!("{:?}", t) == name).map(|(_, w)| w.clone())
    }
    .with_context(|| format!("{} {} has no witnesses for {}", details.name, details.version.version, name))?;
    let base = |image: &PeImage, rva: usize| match base_setter(image, rva) {
        Ok((at, offset)) => format!("its base register was set at +{:X}, {:#X} into what it points to", at, offset),
        Err(why) => why,
    };
    for (rva, matched) in witnesses.iter().zip(match_witnesses(&old, &new, &witnesses)) {
        println!("\nwitness +{:X} ({}):", rva, base(&old, *rva));
        context(&old, *rva).iter().for_each(|line| println!("    {}", line));
        match matched {
            Some(m) => {
                println!(
                    "  matched +{:X} ({:.0}% of the bytes around it), {:#X} -> {:#X} ({}):",
                    m.new_instruction,
                    m.score * 100.0,
                    m.old_displacement,
                    m.new_displacement,
                    base(&new, m.new_instruction)
                );
                context(&new, m.new_instruction).iter().for_each(|line| println!("    {}", line));
            }
            None => println!("  not matched"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(code: &[u8]) -> PeImage {
        let mut data = vec![0xCC_u8; 0x3000];
        data[0x1000..0x1000 + code.len()].copy_from_slice(code);
        PeImage {
            data,
            is_64: true,
            image_base: 0x1_4000_0000,
        }
    }

    #[test]
    fn matches_a_witness_whose_field_moved() {
        let mut code: Vec<u8> = (0..40).map(|i| (i * 7) as u8).collect();
        let old_position = code.len();
        code.extend_from_slice(&[0x0F, 0x28, 0x77, 0x10]); // movaps xmm6, [rdi+0x10]
        code.extend((0..40).map(|i| (i * 13) as u8));
        let old = image(&code);
        code[old_position + 3] = 0x30; // movaps xmm6, [rdi+0x30]
        let new = image(&code);

        let matched = match_witnesses(&old, &new, &[0x1000 + old_position]);
        assert_eq!(
            matched,
            vec![Some(Matched {
                old_displacement: 0x10,
                new_displacement: 0x30,
                score: 1.0,
                new_instruction: 0x1000 + old_position,
            })]
        );
    }
}
