//! `tomb-helper update-offsets`: everything needed to support a new build (or game) of a known executable, in one go.
//! With only the running game it finds what it can on its own: the statics `discover` finds through RTTI, Lara's
//! position through the code chains that reach her instance (the one with a PlayerComponent), the ground probes by
//! their contents, and the version string. Given the previous build (`--from`), it also derives the rest the way
//! `derive` does. It then saves the entry to the offsets file and runs `check` on it.

use crate::chains;
use crate::check;
use crate::components::{self, Components};
use crate::derive::{self, format_offsets, References};
use crate::discover;
use crate::find_process;
use crate::layout;
use crate::object_list::ObjectList;
use crate::offsets_file::{known_process_details, Hex, VersionEntry};
use crate::pe_image::PeImage;
use crate::position::Position;
use crate::process_details::{AddressType, ProcessDetails};
use process_memory::{CopyAddress, Pid, ProcessHandle, TryIntoProcessHandle};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryInto;

// Probes starting this close to Lara are hers
const PROBE_DISTANCE: f32 = 300.0;
const MAX_PROBES: usize = 64;
// Longest version string to read
const VERSION_LENGTH: usize = 64;

pub fn update_offsets(from: Option<String>, from_version: Option<String>, dry_run: bool) -> Result<(), String> {
    let known = known_process_details();
    let (pid, handle, base_address, existing, template) = attach(&known)?;
    layout::set(template.layout.clone());
    match &existing {
        Some(details) => println!("Found {} {}, updating its offsets", details.name, details.version.version),
        None => println!(
            "Found a new build of {} ({}), starting from {}",
            template.name, template.executable_name, template.version.version
        ),
    }
    println!("Dumping the game...");
    let image = PeImage::dump(pid).map_err(|e| format!("Could not dump the game: {}", e))?;

    // How each offset list was found, for the summary
    let mut found: BTreeMap<AddressType, (Vec<usize>, String)> = BTreeMap::new();
    for (address_type, chain) in discover::find_statics(handle, base_address, &image) {
        let how = match template.address_offsets.get(&address_type) {
            Some(previous) if *previous == chain => "RTTI, unchanged",
            Some(_) => "RTTI, moved",
            None => "RTTI, new",
        };
        found.insert(address_type, (chain, how.to_string()));
    }

    let array = found
        .get(&AddressType::InstanceArray)
        .map(|(chain, _)| chain.clone())
        .or_else(|| existing.as_ref().and_then(|d| d.address_offsets.get(&AddressType::InstanceArray).cloned()));
    let lara = array.and_then(|array| find_lara(handle, base_address, array[0]));
    let mut targets = vec![];
    match lara {
        Some((address, position)) => {
            println!("Lara's instance is at {:#X}, at ({:.0}, {:.0}, {:.0})", address, position.x, position.y, position.z);
            // Y and Z follow X, so they use its chain
            targets.push((AddressType::XPosition, address + layout::current().instance.position));
            match find_probes(&image, &position) {
                Some(rva) => {
                    println!("Ground probes at mod+{:X}", rva);
                    found.insert(AddressType::PlayerProbes, (vec![rva], "Lara's probes".to_string()));
                }
                None => println!("Ground probes: NOT FOUND"),
            }
        }
        None => println!("Lara: NOT FOUND (no instance with a PlayerComponent; load into a level)"),
    }
    let version = read_version(handle, base_address, &template);
    if let Some((address, string)) = &version {
        println!("Version string \"{}\"", string);
        targets.push((AddressType::Version, *address));
    }
    // The chains the previous build used are kept if they still get there, otherwise look for the code's
    let mut moved = vec![];
    for (address_type, target) in targets {
        let unchanged = template
            .address_offsets
            .get(&address_type)
            .filter(|chain| resolve(handle, base_address, chain) == Some(target));
        match unchanged {
            Some(chain) => {
                found.insert(address_type, (chain.clone(), "unchanged".to_string()));
            }
            None => moved.push((address_type, target)),
        }
    }
    if !moved.is_empty() {
        let chains = chains::best_code_chains(&image, handle, base_address, template.arch, &moved);
        for (address_type, _) in &moved {
            match chains.get(address_type) {
                Some((chain, sites)) => {
                    found.insert(*address_type, (chain.clone(), format!("code chain, {} sites", sites)));
                }
                None => println!("{:?}: no code chain reaches it", address_type),
            }
        }
    }
    if let Some((x, how)) = found.get(&AddressType::XPosition).cloned() {
        for (address_type, offset) in [(AddressType::YPosition, 4), (AddressType::ZPosition, 8)] {
            let mut chain = x.clone();
            *chain.last_mut().unwrap() += offset;
            found.insert(address_type, (chain, format!("XPosition + {} ({})", offset, how)));
        }
    }

    let mut unverified: BTreeMap<AddressType, Vec<usize>> = BTreeMap::new();
    if let Some(from) = &from {
        let old = derive::load(from)?;
        let old_details = derive::reference_details(&old, from_version.as_deref())?;
        println!("Deriving the rest from {} {}...", old_details.name, old_details.version.version);
        for (address_type, chain, sure, note) in derive::derive_lines(&old, &old_details, &image) {
            match chain {
                Some(chain) if sure && !found.contains_key(&address_type) => {
                    found.insert(address_type, (chain, format!("derived: {}", note)));
                }
                Some(chain) if !found.contains_key(&address_type) => {
                    unverified.insert(address_type, chain);
                }
                _ => {}
            }
        }
    }

    // Anything not found keeps what it was (for a known build), or is left for review (for a new one)
    let mut offsets: BTreeMap<AddressType, Vec<usize>> = BTreeMap::new();
    for (address_type, chain) in &template.address_offsets {
        if found.contains_key(address_type) || unverified.contains_key(address_type) {
            continue;
        }
        if existing.is_some() {
            offsets.insert(*address_type, chain.clone());
            found.insert(*address_type, (chain.clone(), "kept".to_string()));
        } else {
            unverified.insert(*address_type, chain.clone());
        }
    }
    for (address_type, (chain, _)) in &found {
        offsets.insert(*address_type, chain.clone());
    }

    let image_size = image.size_of_image();
    let version_name = match (&existing, &version) {
        (Some(details), _) => details.version.version.clone(),
        (None, Some((_, string))) => format!("{} (image size {})", string, image_size),
        (None, None) => format!("unknown build (image size {})", image_size),
    };
    let hex = |chain: &Vec<usize>| chain.iter().map(|o| Hex(*o)).collect::<Vec<Hex>>();
    let entry = VersionEntry {
        game: template.name.clone(),
        executable: template.executable_name.clone(),
        version: version_name,
        image_size: Some(image_size),
        bits: if image.is_64 { 64 } else { 32 },
        offsets: offsets.iter().map(|(t, c)| (*t, hex(c))).collect(),
        witnesses: existing.as_ref().map_or_else(BTreeMap::new, |d| d.witnesses.iter().map(|(t, w)| (*t, hex(w))).collect()),
        unverified: unverified.iter().map(|(t, c)| (*t, hex(c))).collect(),
        layout: template.layout.clone(),
    };

    println!();
    println!("{} {}:", entry.game, entry.version);
    for (address_type, (chain, how)) in &found {
        println!("  {:<17} {:<44} {}", format!("{:?}", address_type), format_offsets(chain), how);
    }
    for (address_type, chain) in &unverified {
        println!("  {:<17} {:<44} UNVERIFIED, not used", format!("{:?}", address_type), format_offsets(chain));
    }
    if !unverified.is_empty() {
        println!("  (Offsets marked unverified are kept in the file to look at; pass --from with the previous build's");
        println!("  exe or a dump of it to derive them, or check them with `tomb-helper probe` and move them over.)");
    }

    let details = entry.to_details()?;
    if dry_run {
        println!("\n--dry-run, so not saved");
    } else {
        discover::save_entry(entry)?;
    }
    println!("\nChecking it...");
    check::run(handle, base_address, details)
}

/// The running game (a known executable), its known details if this build is known, and the details to start from:
/// the known ones, or the last known build of the same executable
fn attach(
    known: &[ProcessDetails],
) -> Result<(Pid, ProcessHandle, usize, Option<ProcessDetails>, ProcessDetails), String> {
    let mut executables: Vec<&str> = known.iter().map(|d| d.executable_name.as_str()).collect();
    executables.dedup();
    for executable in executables {
        let pid = match find_process::get_pid(executable) {
            Some(pid) => pid,
            None => continue,
        };
        let handle = pid.try_into_process_handle().map_err(|e| format!("Could not open {}: {}", executable, e))?;
        let base_address = find_process::get_base_address(pid) as usize;
        let same_executable: Vec<ProcessDetails> =
            known.iter().filter(|d| d.executable_name == executable).cloned().collect();
        let existing = find_process::find_process(same_executable.clone(), None).map(|(_, _, _, d)| d);
        let mut template = existing.clone().or_else(|| same_executable.last().cloned()).unwrap();
        // An entry from before the overlay has no layout, and the default is another game's, so borrow one from the
        // last build of the same game that has one
        if template.layout.is_default() {
            if let Some(other) = same_executable.iter().rev().find(|d| !d.layout.is_default()) {
                println!("Using the layout of {} to start with", other.version.version);
                template.layout = other.layout.clone();
            }
        }
        return Ok((pid, handle, base_address, existing, template));
    }
    Err(format!("None of the known games ({}) is running", known.iter().map(|d| d.executable_name.clone()).collect::<HashSet<_>>().into_iter().collect::<Vec<_>>().join(", ")))
}

/// Lara's instance (the one with a PlayerComponent) and where she is
fn find_lara(handle: ProcessHandle, base_address: usize, array_rva: usize) -> Option<(usize, Position)> {
    let instances = ObjectList::new(base_address + array_rva).instances(handle).ok()?;
    let mut components = Components::new(base_address);
    components.allow_scans(instances.len());
    instances
        .into_iter()
        .find(|i| components.get(handle, i.address, components::PLAYER).is_some())
        .map(|i| (i.address, i.position))
}

/// The static list of ground probes: a count, then records, at least one of which starts at Lara's feet. Prefers ones
/// the code refers to.
fn find_probes(image: &PeImage, lara: &Position) -> Option<usize> {
    let layout = &layout::current().probes;
    let data = &image.data;
    let float = |offset: usize| data.get(offset..offset + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
    let point = |offset: usize| Some([float(offset)?, float(offset + 4)?, float(offset + 8)?]);
    let length = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let lara = [lara.x, lara.y, lara.z];
    let probes_at = |rva: usize| -> bool {
        let count = match data.get(rva..rva + 4) {
            Some(b) => u32::from_le_bytes(b.try_into().unwrap()) as usize,
            None => return false,
        };
        if count == 0 || count > MAX_PROBES {
            return false;
        }
        let mut hers = false;
        for i in 0..count {
            let record = rva + layout.records + i * layout.size;
            let (start, end) = match (point(record + layout.start), point(record + layout.end)) {
                (Some(s), Some(e)) => (s, e),
                _ => return false,
            };
            let ray = length(start, end);
            if !start.iter().chain(end.iter()).all(|v| v.is_finite() && v.abs() < 1e7) || !(1.0..5000.0).contains(&ray) {
                return false;
            }
            hers |= length(start, lara) < PROBE_DISTANCE;
        }
        hers
    };
    let candidates: Vec<usize> = (0..data.len().saturating_sub(layout.records)).step_by(8).filter(|&rva| probes_at(rva)).collect();
    let references = References::new(image);
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for (target, _) in references.all() {
        *counts.entry(*target as usize).or_insert(0) += 1;
    }
    candidates.into_iter().max_by_key(|rva| counts.get(rva).copied().unwrap_or(0))
}

/// Where the chain points, following the same rules as `TrackedMemory`
fn resolve(handle: ProcessHandle, base_address: usize, chain: &[usize]) -> Option<usize> {
    let mut address = base_address + chain[0];
    for offset in &chain[1..] {
        let mut pointer = [0u8; 8];
        handle.copy_address(address, &mut pointer).ok()?;
        address = (u64::from_le_bytes(pointer) as usize).checked_add(*offset)?;
    }
    Some(address)
}

/// Where the version string is and what it says, if the previous build's `Version` offsets still find one
fn read_version(handle: ProcessHandle, base_address: usize, template: &ProcessDetails) -> Option<(usize, String)> {
    let address = resolve(handle, base_address, template.address_offsets.get(&AddressType::Version)?)?;
    let mut bytes = [0u8; VERSION_LENGTH];
    handle.copy_address(address, &mut bytes).ok()?;
    let end = bytes.iter().position(|&b| b == 0)?;
    let string = String::from_utf8(bytes[..end].to_vec()).ok()?;
    if string.len() >= 4 && string.chars().all(|c| c.is_ascii_graphic() || c == ' ') && string.contains(|c: char| c.is_ascii_digit()) {
        Some((address, string))
    } else {
        None
    }
}
