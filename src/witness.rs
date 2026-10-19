//! Matches "witness" instructions (ones known to access a value, found with `tomb-helper watch`)
//! from one build into another, to see how the field offset they use has changed.

use crate::pe_image::PeImage;
use iced_x86::{Decoder, DecoderOptions};
use rayon::prelude::*;
use std::collections::HashMap;

const WINDOW: usize = 32;
const MIN_SCORE: f64 = 0.6;
const MIN_MARGIN: f64 = 0.1;

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
        &data[..data.len().min(15)],
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

// The two bytes before the displacement (usually opcode + ModRM)
fn key(data: &[u8], position: usize) -> usize {
    (data[position - 2] as usize) << 8 | data[position - 1] as usize
}

fn similarity(a: &[u8], p: usize, b: &[u8], q: usize, size: usize) -> usize {
    let same = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(i, j)| i == j).count();
    same(&a[p - WINDOW..p], &b[q - WINDOW..q]) + same(&a[p + size..p + size + WINDOW], &b[q + size..q + size + WINDOW])
}

/// A witness matched into the new build
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matched {
    pub old_displacement: i64,
    pub new_displacement: i64,
    pub score: f64,
}

/// Matches each witness instruction (by RVA in `old`) into `new`, returning one result per witness
pub fn match_witnesses(old: &PeImage, new: &PeImage, witnesses: &[usize]) -> Vec<Option<Matched>> {
    let old_displacements: Vec<Option<Displacement>> = witnesses.iter().map(|w| displacement_at(old, *w)).collect();

    // One pass over the new image collects every position with a key we're looking for
    let mut wanted = vec![false; 0x10000];
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
            let new_displacement = (q.saturating_sub(14)..q)
                .filter_map(|start| displacement_at(new, start))
                .find(|n| n.position == q && n.size == d.size)?;
            Some(Matched {
                old_displacement: d.value,
                new_displacement: new_displacement.value,
                score: fraction(best.0),
            })
        })
        .collect()
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
                score: 1.0
            })]
        );
    }
}
