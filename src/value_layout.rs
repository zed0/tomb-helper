//! Working out where a structure's fields are in another game (or build) from the values in them, with Lara standing
//! somewhere ordinary in both: the games share their gameplay data (her standing movement capsule is radius 25, 60
//! long, (0, -5, 85) from her feet in ROTTR and SOTTR), so each field's value in the source (a snapshot, or the running
//! game) is looked for in the same object in the target. Positions in the world are compared as how far from Lara
//! they are, and facings (quaternions) by how far from upright they tilt, as she faces somewhere else. Fields keep their
//! order in a structure, so of the places each value is found, the ones kept are those in order, nearest the shift of
//! the fields around them (`assign`). `debug value-layout <snapshot> [<group>]`, against the running game.

use anyhow::{bail, Context, Result};
use crate::components::{self, Components};
use crate::game::Game;
use crate::game_memory::{f32_at, resolve, vector_at, Handle};
use crate::layout::field_offset;
use crate::process_details::{AddressType, ProcessDetails};
use crate::snapshot::Snapshot;
use nalgebra::{Quaternion, UnitQuaternion, Vector3};
use process_memory::CopyAddress;
use std::path::Path;

// How much of the target's object is looked through
const SPAN: usize = 0x400;
// Floats this close are the same value (relative to their size, at least this much)
const TOLERANCE: f32 = 0.01;
const MIN_TOLERANCE: f32 = 0.001;

/// How a field's value is compared
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    // `n` floats, as they are
    Floats(usize),
    // A position in the world: how far from Lara's feet, and how high above them
    Position,
    // A rotation (x, y, z, w): how far it tilts from upright, so not which way it faces
    Rotation,
}

impl Kind {
    fn size(self) -> usize {
        match self {
            Kind::Floats(n) => n * 4,
            Kind::Position => 12,
            Kind::Rotation => 16,
        }
    }
}

/// The fields of a group that can be found by their values, and how each is compared
pub fn fields(group: &str) -> &'static [(&'static str, Kind)] {
    match group {
        "proxy" => &[
            ("proxy.centre", Kind::Position),
            ("proxy.in_use_size", Kind::Floats(2)),
            ("proxy.in_use_offset", Kind::Floats(3)),
            ("proxy.in_use_local_rotation", Kind::Rotation),
            ("proxy.in_use_rotation", Kind::Rotation),
            ("proxy.next_size", Kind::Floats(2)),
            ("proxy.next_offset", Kind::Floats(3)),
            ("proxy.next_local_rotation", Kind::Rotation),
            ("proxy.next_rotation", Kind::Rotation),
            ("proxy.resolved_centre", Kind::Position),
        ],
        _ => &[],
    }
}

// What a field's value says, comparable between games: the floats, or for a position its distance from Lara and
// height above her, or for a rotation its tilt
fn describe(bytes: &[u8], offset: usize, kind: Kind, lara: &Vector3<f32>) -> Option<Vec<f32>> {
    if offset + kind.size() > bytes.len() {
        return None;
    }
    let values = match kind {
        Kind::Floats(n) => (0..n).map(|i| f32_at(bytes, offset + i * 4)).collect(),
        Kind::Position => {
            let relative = vector_at(bytes, offset) - lara;
            vec![relative.norm(), relative.z]
        }
        Kind::Rotation => {
            let [x, y, z, w] = [0, 4, 8, 12].map(|o| f32_at(bytes, offset + o));
            let length = (x * x + y * y + z * z + w * w).sqrt();
            if !(0.99..1.01).contains(&length) {
                return None;
            }
            let rotation = UnitQuaternion::from_quaternion(Quaternion::new(w, x, y, z));
            // Tilt: how far its up axis is from the world's
            vec![(rotation * Vector3::z()).z]
        }
    };
    values.iter().all(|v| v.is_finite()).then(|| values)
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= (TOLERANCE * x.abs().max(y.abs())).max(MIN_TOLERANCE))
}

/// Where a field went, as `assign` worked it out
#[derive(Debug, Clone)]
pub struct Placed {
    pub name: &'static str,
    pub source: usize,
    pub target: Option<usize>,
    // How many places in the target held its value
    pub candidates: usize,
}

/// Of the places each field's value is (`candidates`, by field in source order), the ones that keep the fields in
/// order (not overlapping), as many as can be, and then those nearest the shift of the field before
fn assign(fields: &[(&'static str, usize, Kind, Vec<usize>)]) -> Vec<Placed> {
    // best[i][c]: (fields placed, -total jump in shift, chosen) ending with field i at its candidate c
    let n = fields.len();
    let mut best: Vec<Vec<(usize, i64, Option<(usize, usize)>)>> = vec![];
    for i in 0..n {
        let (_, source, _, candidates) = &fields[i];
        let mut row = vec![];
        for &target in candidates {
            let shift = target as i64 - *source as i64;
            let mut choice = (1, 0i64, None);
            for j in 0..i {
                let (_, other_source, other_kind, other_candidates) = &fields[j];
                for (k, &other) in other_candidates.iter().enumerate() {
                    if other + other_kind.size() > target {
                        continue;
                    }
                    let (count, jumps, _) = best[j][k];
                    let other_shift = other as i64 - *other_source as i64;
                    let candidate = (count + 1, jumps - (shift - other_shift).abs(), Some((j, k)));
                    if (candidate.0, candidate.1) > (choice.0, choice.1) {
                        choice = candidate;
                    }
                }
            }
            row.push(choice);
        }
        best.push(row);
    }
    // Back from the best end
    let mut placed: Vec<Option<usize>> = vec![None; n];
    let end = (0..n)
        .flat_map(|i| (0..best[i].len()).map(move |c| (i, c)))
        .max_by_key(|&(i, c)| (best[i][c].0, best[i][c].1));
    let mut at = end;
    while let Some((i, c)) = at {
        placed[i] = Some(fields[i].3[c]);
        at = best[i][c].2;
    }
    fields
        .iter()
        .zip(placed)
        .map(|((name, source, _, candidates), target)| Placed { name, source: *source, target, candidates: candidates.len() })
        .collect()
}

// Lara's position, and her proxy's address: a static (TR2013's `PlayerProxy`), or her instance's ProxyComponent (her
// instance is what her position chain leads into)
fn lara_proxy(game: &Game, components: &mut Components) -> Option<(Vector3<f32>, usize)> {
    let lara = game.lara_position().ok()?.vector();
    if game.has(AddressType::PlayerProxy) {
        return Some((lara, game.resolve(AddressType::PlayerProxy)?));
    }
    let chain = game.offsets.get(&AddressType::XPosition)?;
    let (_, prefix) = chain.split_last()?;
    let instance = resolve(game.handle, game.base_address, &[prefix, &[0]].concat())?;
    let proxy = components.get(game.handle, instance, components::PROXY)?;
    Some((lara, proxy))
}

// As much of the object as can be read: `SPAN`, or (from a snapshot, which only has what `check` read) the proxy's size
// in the layout
fn read_object(game: &Game, address: usize) -> Option<Vec<u8>> {
    [SPAN, crate::layout::current().proxy.size].iter().find_map(|&size| {
        let mut bytes = vec![0u8; size];
        game.handle.copy_address(address, &mut bytes).ok().map(|_| bytes)
    })
}

/// Where `group`'s fields (in `source`'s layout) are in `target`, by their values. Each game's objects are found with
/// its own layout (which needs only where an instance's components and position are)
pub fn derive(source: &Game, source_details: &ProcessDetails, target: &Game, target_details: &ProcessDetails, group: &str) -> Result<Vec<Placed>> {
    if fields(group).is_empty() {
        bail!("no fields of {} can be found by their values yet", group);
    }
    let found_in = |game: &Game, details: &ProcessDetails, what: &str| {
        crate::layout::set_for(details);
        let mut components = Components::new(game.base_address);
        components.allow_scans(4096);
        lara_proxy(game, &mut components).with_context(|| format!("Lara's proxy wasn't found in the {}", what))
    };
    let (source_lara, source_object) = found_in(source, source_details, "source")?;
    let (target_lara, target_object) = found_in(target, target_details, "target")?;
    let target_bytes = read_object(target, target_object).context("the target's proxy couldn't be read")?;

    let mut found = vec![];
    for &(name, kind) in fields(group) {
        let offset = match field_offset(&source_details.layout, name) {
            Some(offset) => offset,
            None => continue,
        };
        let mut bytes = vec![0u8; kind.size()];
        if source.handle.copy_address(source_object + offset, &mut bytes).is_err() {
            continue;
        }
        let value = match describe(&bytes, 0, kind, &source_lara) {
            Some(value) => value,
            None => continue,
        };
        let candidates: Vec<usize> = (0..target_bytes.len().saturating_sub(kind.size()))
            .step_by(4)
            .filter(|&o| describe(&target_bytes, o, kind, &target_lara).map_or(false, |v| same(&v, &value)))
            .collect();
        found.push((name, offset, kind, candidates));
    }
    found.sort_by_key(|(_, offset, _, _)| *offset);
    Ok(assign(&found))
}

/// The snapshots to take values from for a new build of `template`'s game: the newest of the build it's derived from
/// (`previous`), else of another build of the same game, else (for a new game) the newest of each other game
pub fn sources(template: &ProcessDetails, previous: Option<&ProcessDetails>, known: &[ProcessDetails]) -> Vec<(Game, ProcessDetails)> {
    let mut snapshots: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(crate::snapshot::snapshot_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.extension().map_or(false, |e| e == "snap"))
        .collect();
    snapshots.sort();
    snapshots.reverse();
    let named = |prefix: &str| -> Option<std::path::PathBuf> {
        snapshots.iter().map(|(_, p)| p).find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with(prefix))).cloned()
    };
    let this_build = crate::snapshot::file_name(&template.name, &template.version.version) + " ";
    let chosen: Vec<std::path::PathBuf> = match previous {
        Some(previous) => named(&(crate::snapshot::file_name(&previous.name, &previous.version.version) + " ")).into_iter().collect(),
        None if known.iter().any(|d| d.name == template.name) => snapshots
            .iter()
            .map(|(_, p)| p)
            .find(|p| {
                let name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string());
                name.starts_with(&crate::snapshot::file_name(&template.name, "")) && !name.starts_with(&this_build)
            })
            .cloned()
            .into_iter()
            .collect(),
        None => {
            let mut games: Vec<&str> = known.iter().map(|d| d.name.as_str()).filter(|n| *n != template.name).collect();
            games.dedup();
            games.iter().filter_map(|game| named(&crate::snapshot::file_name(game, ""))).collect()
        }
    };
    chosen.iter().filter_map(|path| snapshot_game(&path.to_string_lossy()).ok()).collect()
}

/// The layout with the proxy's fields where their values say they are in `target`, from each of `sources` (which
/// have to agree). Only fields the layout has are moved (values say where a field went, not that the game uses it the
/// same way), and not those in `settled`, which says which were changed.
pub fn apply(
    mut layout: crate::layout::Layout,
    sources: &[(Game, ProcessDetails)],
    target: &Game,
    target_details: &ProcessDetails,
    settled: &mut std::collections::HashSet<String>,
    notes: &mut Vec<String>,
) -> crate::layout::Layout {
    let mut said: std::collections::BTreeMap<&'static str, Vec<(usize, String)>> = Default::default();
    for (source, details) in sources {
        let target_details = ProcessDetails { layout: layout.clone(), ..target_details.clone() };
        match derive(source, details, target, &target_details, "proxy") {
            Ok(placed) => {
                for p in placed {
                    if let Some(offset) = p.target {
                        said.entry(p.name).or_default().push((offset, details.version.version.clone()));
                    }
                }
            }
            Err(e) => notes.push(format!("{:<34} couldn't be compared with {}: {:#}", "proxy", details.version.version, e)),
        }
    }
    crate::layout::set(layout.clone());
    for (name, says) in said {
        let current = field_offset(&layout, name);
        if settled.contains(name) || current.is_none() {
            continue;
        }
        let offsets: std::collections::HashSet<usize> = says.iter().map(|(o, _)| *o).collect();
        let versions: Vec<&str> = says.iter().map(|(_, v)| v.as_str()).collect();
        if offsets.len() > 1 {
            notes.push(format!("{:<34} UNSURE, its value is in different places by {}, kept {:#X}", name, versions.join(", "), current.unwrap_or(0)));
            continue;
        }
        let offset = says[0].0;
        if current == Some(offset) {
            continue;
        }
        if let Ok(changed) = crate::layout::with_field(&layout, name, offset) {
            layout = changed;
            settled.insert(name.to_string());
            notes.push(format!("{:<34} {:#X} -> {:#X} (the value in it, as in {})", name, current.unwrap_or(0), offset, versions.join(", ")));
        }
    }
    layout
}

// A snapshot's game and its version's details
fn snapshot_game(path: &str) -> Result<(Game, ProcessDetails)> {
    let snapshot = Snapshot::load(Path::new(path)).with_context(|| format!("couldn't load {}", path))?;
    let snapshot: &'static Snapshot = Box::leak(Box::new(snapshot));
    let details = snapshot.details()?;
    Ok((Game::new(Handle::Snapshot(snapshot), snapshot.header.base_address, &details.address_offsets), details))
}

/// `debug value-layout <source snapshot> [<group>] [--to <snapshot>]`: the running game's (or the other snapshot's)
/// layout for the group, from the snapshot's
pub fn print_value_layout(snapshot: &str, group: &str, version: Option<String>, to: Option<&str>) -> Result<()> {
    let (source, source_details) = snapshot_game(snapshot)?;
    let (target, target_details) = match to {
        Some(to) => snapshot_game(to)?,
        None => {
            let connection = crate::chains::connect(version)?;
            (connection.game(), connection.details)
        }
    };
    println!("From {} to {}", source_details, target_details);
    let placed = derive(&source, &source_details, &target, &target_details, group)?;
    let (mut right, mut wrong, mut unplaced) = (0, 0, 0);
    for p in &placed {
        let known = field_offset(&target_details.layout, p.name);
        let verdict = match (p.target, known) {
            (Some(t), Some(k)) if t == k => {
                right += 1;
                "right"
            }
            (Some(_), Some(_)) => {
                wrong += 1;
                "WRONG"
            }
            (None, _) => {
                unplaced += 1;
                "-"
            }
            _ => "",
        };
        println!(
            "  {:<30} {:>6} -> {:>6}  {:<6} {} places held its value{}",
            p.name,
            format!("{:#X}", p.source),
            p.target.map_or("?".to_string(), |t| format!("{:#X}", t)),
            verdict,
            p.candidates,
            known.filter(|k| Some(*k) != p.target).map_or(String::new(), |k| format!(" (it's {:#X})", k)),
        );
    }
    println!("{} right, {} wrong, {} not placed", right, wrong, unplaced);

    // What `update-offsets` would do with a layout whose proxy is still the source's (as a template from another build)
    let mut stale = target_details.layout.clone();
    for (name, _) in fields(group) {
        if let (Some(_), Some(offset)) = (field_offset(&stale, name), field_offset(&source_details.layout, name)) {
            stale = crate::layout::with_field(&stale, name, offset)?;
        }
    }
    let mut notes = vec![];
    let fixed = apply(stale, &[(source, source_details)], &target, &target_details, &mut Default::default(), &mut notes);
    println!("\nupdate-offsets would set, from the source's proxy:");
    for note in &notes {
        let name = note.split_whitespace().next().unwrap_or("");
        let right = field_offset(&fixed, name) == field_offset(&target_details.layout, name);
        println!("  {:<6} {}", if right { "right" } else { "WRONG" }, note);
    }
    let all = fields(group).iter().all(|(name, _)| field_offset(&fixed, name) == field_offset(&target_details.layout, name));
    println!("{}", if all { "Every field is where the target's layout has it" } else { "Some fields AREN'T where the target's layout has them" });
    Ok(())
}
