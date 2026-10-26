//! Finding where the height Lara last left the ground at is (the fall start, which fall damage goes by, and flight
//! mode moves with her), by lifting her: put her a little way up in the air and the game sets it to the height she was
//! put at as she falls, so the fields that follow two lifts of different heights are it (SOTTR has two copies). It's
//! looked for in her player object (RTTI `Player`), found from her instance a few pointers in, and only works while
//! the game runs (it pauses when it isn't in front). The drops are small, and she lands where she was.
//! `debug find-fall-start [--delay N]`.

use anyhow::{bail, Context, Result};
use crate::game::Game;
use crate::game_memory::{f32_at, pointer_from, pointer_size, resolve, Handle};
use crate::process_details::AddressType;
use crate::rtti::Classes;
use crate::tracked_memory::TrackedMemory;
use process_memory::{Architecture, CopyAddress, ProcessHandle};
use std::time::Duration;

pub const PLAYER: &str = ".?AVPlayer@@";
// How high she's lifted each time: small enough to land without harm
const LIFTS: [f32; 2] = [150.0, 300.0];
// How long to leave her to fall and land, and how soon she should have started falling if the game's running
const FALL_TIME: Duration = Duration::from_millis(2500);
const MOVING_TIME: Duration = Duration::from_millis(300);
// How much of the player object is looked through, and how far into her instance and what it points to for it
const PLAYER_SPAN: usize = 0x4000;
const SEARCH_SPAN: usize = 0x200;
// A fall start this close to the height she was lifted to is it
const TOLERANCE: f32 = 20.0;

/// Her player object: the object of class `Player` her instance points to, directly or through one more pointer
pub fn find_player(game: &Game, instance: usize) -> Option<(usize, Vec<usize>)> {
    let mut classes = Classes::new(game.base_address);
    let size = pointer_size();
    let pointers = |address: usize| -> Vec<(usize, usize)> {
        let mut bytes = vec![0u8; SEARCH_SPAN];
        if game.handle.copy_address(address, &mut bytes).is_err() {
            return vec![];
        }
        (0..SEARCH_SPAN).step_by(size).map(|o| (o, pointer_from(&bytes[o..]))).filter(|(_, p)| *p != 0).collect()
    };
    for (first, object) in pointers(instance) {
        if classes.name(game.handle, object).as_deref() == Some(PLAYER) {
            return Some((object, vec![first]));
        }
        for (second, inner) in pointers(object) {
            if classes.name(game.handle, inner).as_deref() == Some(PLAYER) {
                return Some((inner, vec![first, second]));
            }
        }
    }
    None
}

// Lara's instance: what her position chain leads into
fn lara_instance(game: &Game) -> Option<usize> {
    let chain = game.offsets.get(&AddressType::XPosition)?;
    let (_, prefix) = chain.split_last()?;
    resolve(game.handle, game.base_address, &[prefix, &[0]].concat())
}

/// What `find` found: the player object and how it was reached from her instance, and the offsets in it that followed
/// both lifts
pub struct Found {
    pub player: usize,
    pub path: Vec<usize>,
    pub offsets: Vec<usize>,
}

/// The layout with the fall start (and its copy, if there are two) where `found` says, and the path from her instance
/// to the player where her position chain doesn't lead to it already
pub fn with_found(game: &Game, mut layout: crate::layout::Layout, found: &Found) -> crate::layout::Layout {
    layout.player.fall_start = found.offsets.first().copied();
    layout.player.fall_start_copy = found.offsets.get(1).copied();
    // Through the position chain (where the player points at her at `player.instance`), if that gets there
    let chain = game.offsets.get(&AddressType::XPosition).cloned().unwrap_or_default();
    let through_chain = (chain.len() >= 3 && Some(chain[chain.len() - 2]) == layout.player.instance)
        .then(|| resolve(game.handle, game.base_address, &[&chain[..chain.len() - 2], &[0]].concat()))
        .flatten();
    if through_chain != Some(found.player) {
        layout.player.instance = None;
        layout.player.from_instance = found.path.clone();
    }
    layout
}

/// Lifts Lara twice and finds the fields of her player object that followed
pub fn find(game: &Game, handle: ProcessHandle, arch: Architecture) -> Result<Found> {
    let instance = lara_instance(game).context("Lara's instance wasn't found through her position chain")?;
    let (player, path) = find_player(game, instance).context("her player object wasn't found from her instance")?;
    let z_chain = game.offsets.get(&AddressType::ZPosition).context("the version has no ZPosition")?.clone();
    let mut z = TrackedMemory::<f32>::new(0.0, z_chain, arch, game.base_address);
    let read_player = || -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; PLAYER_SPAN];
        game.handle.copy_address(player, &mut bytes).ok().map(|_| bytes)
    };
    let mut followed: Option<Vec<usize>> = None;
    for lift in LIFTS {
        z.fetch_from_game(handle).context("her height couldn't be read")?;
        let standing = z.data;
        let lifted = standing + lift;
        z.data = lifted;
        z.apply_to_game(handle).context("she couldn't be lifted")?;
        std::thread::sleep(MOVING_TIME);
        z.fetch_from_game(handle)?;
        if (z.data - lifted).abs() < 0.01 {
            // Put her back where she was
            z.data = standing;
            let _ = z.apply_to_game(handle);
            bail!("she didn't fall after being lifted, so the game isn't running: switch to it while this runs");
        }
        std::thread::sleep(FALL_TIME - MOVING_TIME);
        let bytes = read_player().context("her player object couldn't be read")?;
        let here: Vec<usize> =
            (0..PLAYER_SPAN - 3).step_by(4).filter(|&o| (f32_at(&bytes, o) - lifted).abs() <= TOLERANCE).collect();
        followed = Some(match followed {
            Some(before) => before.into_iter().filter(|o| here.contains(o)).collect(),
            None => here,
        });
    }
    let offsets = followed.unwrap_or_default();
    if offsets.is_empty() {
        bail!("no field of her player object followed both lifts");
    }
    Ok(Found { player, path, offsets })
}

/// `debug find-fall-start [--delay N] [--save]` (and `port`)
pub fn find_fall_start(version: Option<String>, delay: u64, save: bool) -> Result<()> {
    let (pid, handle, base_address, mut details) = crate::chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    if delay > 0 {
        println!("Switch to the game: lifting Lara in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let game = Game::new(Handle::Live(pid, handle), base_address, &details.address_offsets);
    let found = find(&game, handle, details.arch)?;
    let path: Vec<String> = found.path.iter().map(|o| format!("{:#X}", o)).collect();
    println!("Her player object is [instance + {}]", path.join("] + "));
    let known = [details.layout.player.fall_start, details.layout.player.fall_start_copy];
    for offset in &found.offsets {
        let note = if known.contains(&Some(*offset)) { " (the layout's)" } else { "" };
        println!("  +{:#X} followed both lifts{}", offset, note);
    }
    if found.offsets.len() > 2 {
        println!("More than two followed, so only the first two are kept (the fall start, and its copy)");
    }
    if save {
        details.layout = with_found(&game, details.layout.clone(), &found);
        crate::discover::save_entry(crate::offsets_file::VersionEntry::from_details(&details))?;
    }
    Ok(())
}
