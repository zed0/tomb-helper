//! Finding a build's `CutsceneId` while a cutscene plays, for when deriving it from another build isn't sure (the code
//! reaches the id through a pointer to a small record holding it, which says nothing about where that record is). The cutscene's length (`CutsceneLength`, which derives fine) picks its entry in the cutscene timing
//! file, and a cutscene's ids are the same in every build, so the id is the one place, reached the way the chain it's
//! given is (keeping as much of its start as can be kept), that holds one of that entry's ids. Places in a run of known
//! ids (the level's table of its cutscenes) are left out. `debug find-cutscene-id [--save]`, and `port` runs it.

use anyhow::{bail, Context, Result};
use crate::cutscene_timing_info::TimingInfo;
use crate::game::Game;
use crate::game_memory::{resolve, u32_at};
use crate::process_details::AddressType;
use crate::readable_from_path::ReadableFromPath;
use crate::offsets_file::describe_chain;
use process_memory::CopyAddress;
use std::collections::{BTreeMap, HashSet};

// How close (in seconds) the timing file's length has to be to the cutscene's
const LENGTH_TOLERANCE: f32 = 0.5;
// How much of each object is looked through
const SPAN: usize = 0x1000;
// How many of the chain's last offsets can be looked for at most (the rest is kept)
const MAX_SEARCHED: usize = 2;
// How far either side of a candidate a known id means it's in a table of them, not the id of what's playing
const RUN: usize = 8;

/// A cutscene id found in the game
#[derive(Debug, Clone)]
pub struct Found {
    pub chain: Vec<usize>,
    pub id: u32,
    pub how: String,
}

/// Where the id of the cutscene that's playing is, starting from `hint` (another build's chain, or this one's unsure
/// one), with the timing file's cutscenes
pub fn find(game: &Game, hint: &[usize], timing: &TimingInfo) -> Result<Found> {
    let [length] = game.read_floats::<1>(AddressType::CutsceneLength).context("there's no cutscene playing")?;
    let entries: Vec<_> = timing.cutscenes.iter().filter(|e| (e.real_time - length).abs() <= LENGTH_TOLERANCE).collect();
    if entries.is_empty() {
        bail!("no cutscene in the timing file is {:.1}s long, as this one is", length);
    }
    let expected: HashSet<u32> = entries.iter().flat_map(|e| e.ids.iter().copied()).collect();
    let known: HashSet<u32> = timing.cutscenes.iter().flat_map(|e| e.ids.iter().copied()).collect();
    // Keep as much of the chain as can be, looking for more of it until one place holds the cutscene's id
    for kept in (1..hint.len()).rev() {
        if hint.len() - kept > MAX_SEARCHED {
            break;
        }
        let found = search(game, &hint[..kept], hint.len() - kept, &expected, &known);
        let addresses: BTreeMap<usize, Vec<(Vec<usize>, u32)>> = found.into_iter().fold(BTreeMap::new(), |mut by, (a, c, id)| {
            by.entry(a).or_default().push((c, id));
            by
        });
        match addresses.len() {
            0 => continue,
            1 => {
                let (_, chains) = addresses.into_iter().next().unwrap();
                // The one most like the chain it started from: the most offsets the same, then the nearest ones, level
                // by level (the earlier ones say which object it goes through)
                let same = |chain: &Vec<usize>| chain.iter().zip(hint).filter(|(a, b)| a == b).count();
                let distances = |chain: &Vec<usize>| chain.iter().zip(hint).map(|(a, b)| a.abs_diff(*b)).collect::<Vec<_>>();
                let (chain, id) = chains
                    .into_iter()
                    .max_by_key(|(chain, _)| (same(chain), std::cmp::Reverse(distances(chain))))
                    .unwrap();
                let how = format!(
                    "the {:.1}s cutscene's id, kept {} of {}'s offsets",
                    length,
                    kept,
                    describe_chain(hint)
                );
                return Ok(Found { chain, id, how });
            }
            n => {
                let each: Vec<String> = addresses.iter().map(|(a, c)| format!("{:#X} ({:#X})", a, c[0].1)).collect();
                bail!("{} places hold the {:.1}s cutscene's id: {}", n, length, each.join(", "));
            }
        }
    }
    bail!("nothing reached like {} holds the {:.1}s cutscene's id", describe_chain(hint), length)
}

// Every place reached by `prefix` and then `levels` more offsets that holds one of the `expected` ids, and isn't next
// to other `known` ones: (address, chain, id)
fn search(game: &Game, prefix: &[usize], levels: usize, expected: &HashSet<u32>, known: &HashSet<u32>) -> Vec<(usize, Vec<usize>, u32)> {
    // `prefix ++ [y]` is y into what `prefix` leads to points to
    let object = match resolve(game.handle, game.base_address, &[prefix, &[0]].concat()) {
        Some(object) => object,
        None => return vec![],
    };
    if levels == 1 {
        return ids_in(game, object, expected, known)
            .into_iter()
            .map(|(offset, id)| (object + offset, [prefix, &[offset]].concat(), id))
            .collect();
    }
    let bytes = match read(game, object) {
        Some(bytes) => bytes,
        None => return vec![],
    };
    let size = game.handle.pointer_size();
    let mut found = vec![];
    for x in (0..bytes.len().saturating_sub(size)).step_by(size) {
        if game.handle.pointer_from(&bytes[x..]) == 0 {
            continue;
        }
        let next: Vec<usize> = [prefix, &[x]].concat();
        found.extend(search(game, &next, levels - 1, expected, known));
    }
    found
}

// The offsets in the object holding one of the `expected` ids, and not next to other `known` ones
fn ids_in(game: &Game, object: usize, expected: &HashSet<u32>, known: &HashSet<u32>) -> Vec<(usize, u32)> {
    let bytes = match read(game, object) {
        Some(bytes) => bytes,
        None => return vec![],
    };
    let at = |offset: usize| (offset + 4 <= bytes.len()).then(|| u32_at(&bytes, offset));
    (0..bytes.len().saturating_sub(3))
        .step_by(4)
        .filter_map(|offset| {
            let id = at(offset)?;
            let in_run = (4..=RUN).step_by(4).any(|d| {
                offset.checked_sub(d).and_then(at).is_some_and(|v| known.contains(&v)) || at(offset + d).is_some_and(|v| known.contains(&v))
            });
            (expected.contains(&id) && !in_run).then_some((offset, id))
        })
        .collect()
}

// As much of the object as can be read, up to `SPAN`
fn read(game: &Game, address: usize) -> Option<Vec<u8>> {
    [SPAN, SPAN / 16].iter().find_map(|&size| {
        let mut bytes = vec![0u8; size];
        game.handle.copy_address(address, &mut bytes).ok().map(|_| bytes)
    })
}

/// The chain to start from: the version's `CutsceneId`, sure or not
pub fn hint(details: &crate::process_details::ProcessDetails) -> Option<Vec<usize>> {
    details
        .address_offsets
        .get(&AddressType::CutsceneId)
        .or_else(|| details.unverified.get(&AddressType::CutsceneId))
        .cloned()
}

/// Finds the playing cutscene's id in the running game, and with `save` keeps it in the version's entry
pub fn find_and_save(game: &Game, details: &mut crate::process_details::ProcessDetails, save: bool) -> Result<Found> {
    let hint = hint(details).context("the version has no CutsceneId to start from, sure or not")?;
    let config = crate::config::get_config();
    let timing = TimingInfo::from_path(&config.cutscene_timing_file, "cutscene timing info");
    let found = find(game, &hint, &timing)?;
    let unchanged = details.address_offsets.get(&AddressType::CutsceneId) == Some(&found.chain);
    println!("CutsceneId {} = {:#X} ({}){}", describe_chain(&found.chain), found.id, found.how, if unchanged { ", as it was" } else { "" });
    if save && !unchanged {
        details.address_offsets.insert(AddressType::CutsceneId, found.chain.clone());
        details.unverified.remove(&AddressType::CutsceneId);
        crate::offsets_file::save(details)?;
    }
    Ok(found)
}

/// `debug find-cutscene-id [--save]`
pub fn find_cutscene_id(version: Option<String>, save: bool) -> Result<()> {
    let mut connection = crate::chains::connect(version)?;
    let game = connection.game();
    find_and_save(&game, &mut connection.details, save).map(|_| ())
}
