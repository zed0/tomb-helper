//! `tomb-helper update-offsets`: everything needed to support a new build (or game) of a known executable, in one go.
//! With only the running game it finds what it can on its own: the statics `discover` finds through RTTI, Lara's
//! position through the code chains that reach her instance (the one with a PlayerComponent), the ground probes by
//! their contents, and the version string. Given the previous build (`--from`), it also derives the rest the way
//! `derive` does. It then saves the entry to the offsets file and runs `check` on it.
//!
//! The same works for 32-bit games (TR2013), whose Lara is found through her proxy (a static there) rather than a
//! PlayerComponent. Statics with nothing to find them by (TR2013's inventory and type pools) come from `--from`.
//!
//! A new build starts from the last known entry for its exe (one without a layout borrows the last of the same game
//! that has one). Lara's position chain is kept if it still lands on her, else the best code chain (Y and Z are X + 4
//! and 8); what can't be confirmed goes to `unverified`. Saves with `discover::save_entry`, then runs `check::run`.

use anyhow::{anyhow, Context, Result};
use crate::game_memory::{resolve, Handle};
use crate::chains;
use crate::check;
use crate::components::{self, Components};
use crate::derive::{self, format_offsets, References};
use crate::discover;
use crate::find_process;
use crate::layout::{self, Layout};
use crate::object_list::ObjectList;
use crate::offsets_file::{known_process_details, Hex, VersionEntry};
use crate::pe_image::PeImage;
use crate::position::Position;
use crate::process_details::{AddressType, ProcessDetails, VersionIdentifier};
use process_memory::{Architecture, CopyAddress, Pid, ProcessHandle, TryIntoProcessHandle};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryInto;
use std::path::PathBuf;

// Probes starting this close to Lara are hers
const PROBE_DISTANCE: f32 = 300.0;
const MAX_PROBES: usize = 64;
// Longest version string to read
const VERSION_LENGTH: usize = 64;

/// `update-offsets`: fails if the checks do
pub fn update_offsets(from: Option<String>, from_version: Option<String>, dry_run: bool, new_game: Option<NewGame>) -> Result<()> {
    run(from, from_version, dry_run, new_game.as_ref())?.1
}

/// Finds, saves (unless `dry_run`) and checks the running build's offsets: the entry, and how the checks went
pub fn run(
    from: Option<String>,
    from_version: Option<String>,
    dry_run: bool,
    new_game: Option<&NewGame>,
) -> Result<(ProcessDetails, Result<()>)> {
    let known = known_process_details();
    let (pid, handle, base_address, existing, template) = attach(&known, new_game)?;
    layout::set_for(&template);
    match &existing {
        Some(details) => println!("Found {} {}, updating its offsets", details.name, details.version.version),
        None => println!(
            "Found a new build of {} ({}), starting from {}",
            template.name, template.executable_name, template.version.version
        ),
    }
    println!("Dumping the game...");
    let image = PeImage::dump(pid).context("Could not dump the game")?;

    // How each offset list was found, for the summary
    let mut found: BTreeMap<AddressType, (Vec<usize>, String)> = BTreeMap::new();
    let memory = Handle::Live(pid, handle);
    let statics = discover::find_statics(memory, base_address, &image);
    for (address_type, chain) in statics {
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
    // Where her proxy is a static (TR2013), it says which instance she is, and where in it that is
    let mut layout = template.layout.clone();
    let mut layout_notes: Vec<String> = vec![];
    let proxy = layout
        .proxy
        .instance
        .filter(|_| found.contains_key(&AddressType::PlayerProxy))
        .and_then(|offset| discover::find_player_proxy(memory, &image, base_address, offset));
    if let Some(proxy) = &proxy {
        if layout.proxy.instance != Some(proxy.instance_offset) {
            layout_notes.push(format!(
                "{:<34} {:#X} -> {:#X} (where Lara's proxy points at her)",
                "proxy.instance",
                layout.proxy.instance.unwrap_or(0),
                proxy.instance_offset
            ));
            layout.proxy.instance = Some(proxy.instance_offset);
        }
    }
    let proxy_lara = proxy.map(|p| p.lara);
    let lara = match proxy_lara {
        Some(address) => crate::object_list::read_position(memory, address).ok().map(|position| (address, position)),
        None => array.and_then(|array| find_lara(memory, base_address, array[0])),
    };
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
        None => println!("Lara: NOT FOUND (no instance with a PlayerComponent or proxy; load into a level)"),
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
    let previous = match &from {
        Some(from) => {
            let old = derive::load(from)?;
            let old_details = derive::reference_details(&old, from_version.as_deref())?;
            Some((old, old_details))
        }
        None => previous_build(&template, &image),
    };
    // Layout fields already worked out, which later steps don't overrule
    let mut settled: HashSet<String> = HashSet::new();
    if let Some((old, old_details)) = &previous {
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

    // A new build's (or game's) layout, where its classes' code says where their fields are: from the previous build,
    // or for a new game, other games of the engine
    if existing.is_none() {
        let is_new_game = !known.iter().any(|d| d.executable_name.eq_ignore_ascii_case(&template.executable_name));
        let others = if is_new_game { other_games(&image) } else { vec![] };
        let references: Vec<(&PeImage, &ProcessDetails)> = match &previous {
            Some((old, old_details)) => vec![(old, old_details)],
            None => others.iter().map(|(image, details)| (image, details)).collect(),
        };
        if !references.is_empty() {
            let names: Vec<String> = references.iter().map(|(_, d)| format!("{} {}", d.name, d.version.version)).collect();
            println!("Working out the layout from how its classes' code uses it in {}...", names.join(", "));
            // Fields placed between moved ones are only right within a game (`--from` can be another game's dump)
            let same_game = references.iter().all(|(_, d)| d.name == template.name);
            layout = class_layout(layout, &references, &image, same_game, &mut settled, &mut layout_notes);
        }

        // Lara's proxy's fields by the values in them (she's standing somewhere ordinary, as for everything here):
        // the games share their gameplay data, so the same values are in the same fields, wherever those are
        let sources = crate::value_layout::sources(&template, previous.as_ref().map(|(_, d)| d), &known);
        if !sources.is_empty() {
            let names: Vec<String> = sources.iter().map(|(_, d)| format!("{} {}", d.name, d.version.version)).collect();
            println!("Working out Lara's proxy from the values in it, as in {}...", names.join(", "));
            let offsets: crate::process_details::AddressOffsets = found.iter().map(|(t, (chain, _))| (*t, chain.clone())).collect();
            let target = crate::game::Game::new(memory, base_address, &offsets);
            let target_details = ProcessDetails { address_offsets: offsets.clone(), layout: layout.clone(), ..template.clone() };
            layout = crate::value_layout::apply(layout, &sources, &target, &target_details, &mut settled, &mut layout_notes);
            crate::layout::set_for(&ProcessDetails { layout: layout.clone(), ..template.clone() });
        }

        // The camera's fields by what they have to be (no other build needed), moving the camera's chains with them
        let offsets: crate::process_details::AddressOffsets = found.iter().map(|(t, (chain, _))| (*t, chain.clone())).collect();
        let game = crate::game::Game::new(memory, base_address, &offsets);
        layout = crate::camera_fields::apply(layout, &game, &mut found, &mut layout_notes);
        crate::layout::set_for(&ProcessDetails { layout: layout.clone(), ..template.clone() });
    }

    // Anything not found keeps what it was (for a known build), or is left for review (for a new one)
    let mut offsets: BTreeMap<AddressType, Vec<usize>> = BTreeMap::new();
    for (address_type, chain) in &template.address_offsets {
        // An unsure guess doesn't replace what a known build has, but is kept to look at
        if found.contains_key(address_type) || (existing.is_none() && unverified.contains_key(address_type)) {
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
    // What the chains found say of the layout
    let (with_chains, note) = crate::targets::layout_from_chains(layout, &offsets.clone().into_iter().collect());
    layout = with_chains;
    layout_notes.extend(note);

    let image_size = image.size_of_image();
    let version_name = match (&existing, &version, new_game.and_then(|new| new.version.clone())) {
        (Some(details), _, _) => details.version.version.clone(),
        (None, _, Some(named)) => named,
        (None, Some((_, string)), None) => format!("{} (image size {})", string, image_size),
        (None, None, None) => format!("unknown build (image size {})", image_size),
    };
    let hex = |chain: &Vec<usize>| chain.iter().map(|o| Hex(*o)).collect::<Vec<Hex>>();
    let entry = VersionEntry {
        game: template.name.clone(),
        executable: template.executable_name.clone(),
        version: version_name,
        image_size: Some(image_size),
        bits: image.bitness(),
        offsets: offsets.iter().map(|(t, c)| (*t, hex(c))).collect(),
        unverified: unverified.iter().map(|(t, c)| (*t, hex(c))).collect(),
        layout,
    };

    println!();
    println!("{} {}:", entry.game, entry.version);
    for (address_type, (chain, how)) in &found {
        println!("  {:<17} {:<44} {}", format!("{:?}", address_type), format_offsets(chain), how);
    }
    for (address_type, chain) in &unverified {
        println!("  {:<17} {:<44} UNVERIFIED, not used", format!("{:?}", address_type), format_offsets(chain));
    }
    if !layout_notes.is_empty() {
        println!("  Layout:");
        for note in &layout_notes {
            println!("    {}", note);
        }
    }
    if !unverified.is_empty() {
        println!("  (Offsets marked unverified are kept in the file to look at; pass --from with the previous build's");
        println!("  exe or a dump of it to derive them, or check them with `tomb-helper debug probe` and move them over.)");
    }

    let details = entry.to_details()?;
    if dry_run {
        println!("\n--dry-run, so not saved");
    } else {
        discover::save_entry(entry)?;
        save_dump(&details, &image);
    }
    println!("\nChecking it...");
    let checked = check::run(Handle::Live(pid, handle), base_address, details.clone());
    Ok((details, checked))
}

/// Keeps the build's image for the next build's `--from` (a Denuvo build's exe can't be read from disk), unless there's
/// one already. Not being able to is only a warning.
fn save_dump(details: &ProcessDetails, image: &PeImage) {
    let name = crate::snapshot::file_name(&details.name, &details.version.version);
    let path = crate::snapshot::dump_dir().join(format!("{}.bin", name));
    if path.exists() {
        return;
    }
    match std::fs::create_dir_all(crate::snapshot::dump_dir()).and_then(|_| std::fs::write(&path, &image.data)) {
        Ok(()) => println!("Saved a dump of this build for the next one's --from: {}", path.display()),
        Err(e) => eprintln!("Couldn't save a dump of this build to {}: {}", path.display(), e),
    }
}

/// Without `--from`: the newest dump of another known build of the same game in the data folder (`save_dump`'s, or
/// named after the exe as `debug dump` does), if there's one
fn previous_build(template: &ProcessDetails, image: &PeImage) -> Option<(PeImage, ProcessDetails)> {
    let prefixes = [
        crate::snapshot::file_name(&template.name, ""),
        template.executable_name.trim_end_matches(".exe").to_string() + "-",
    ];
    let mut dumps: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(crate::snapshot::dump_dir())
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.ends_with(".bin") && prefixes.iter().any(|p| name.starts_with(p.as_str()))
        })
        // This build's own dump is the same size as its image
        .filter(|e| e.metadata().map_or(false, |m| m.len() != image.data.len() as u64))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dumps.sort();
    for (_, path) in dumps.iter().rev() {
        let old = match PeImage::load(path) {
            Ok(old) => old,
            Err(_) => continue,
        };
        if let Ok(details) = derive::reference_details(&old, None) {
            println!("Using the dump of {} as --from: {}", details.version.version, path.display());
            return Some((old, details));
        }
    }
    if !dumps.is_empty() {
        println!("None of the dumps of {} in {} is of a known build, so not deriving from one", template.name, crate::snapshot::dump_dir().display());
    }
    None
}

/// The layout with the fields the classes' code in `references` says where they are in `image` (`class_fields`):
/// matched by the code, or with `same_game` placed between fields that moved together (which is only reliable within a
/// game). Where several references say, they have to agree. Fields in `settled` are left as they are, and
/// those changed are added to it. Only fields the layout has are moved: another game having a field doesn't say this
/// one uses it the same way.
pub fn class_layout(
    mut layout: Layout,
    references: &[(&PeImage, &ProcessDetails)],
    image: &PeImage,
    same_game: bool,
    settled: &mut HashSet<String>,
    notes: &mut Vec<String>,
) -> Layout {
    use crate::class_fields::Confidence;
    // Field -> (where it is, which reference said, how)
    let mut said: BTreeMap<String, Vec<(usize, String, String)>> = BTreeMap::new();
    for (reference, details) in references {
        for field in crate::class_fields::derive_layout(reference, &details.layout, image) {
            let usable = field.confidence == Confidence::Matched || (same_game && field.confidence == Confidence::Placed);
            if let (true, Some(new)) = (usable, field.new) {
                said.entry(field.name).or_default().push((new, details.version.version.clone(), field.how));
            }
        }
    }
    for (name, says) in said {
        if settled.contains(&name) {
            continue;
        }
        let current = crate::layout::field_offset(&layout, &name);
        if current.is_none() {
            continue;
        }
        let offsets: HashSet<usize> = says.iter().map(|(offset, _, _)| *offset).collect();
        if offsets.len() > 1 {
            let each: Vec<String> = says.iter().map(|(offset, version, _)| format!("{:#X} in {}", offset, version)).collect();
            notes.push(format!("{:<34} UNSURE, the classes' code disagrees ({}), kept {:#X}", name, each.join(", "), current.unwrap_or(0)));
            continue;
        }
        let (offset, _, how) = &says[0];
        if current == Some(*offset) {
            continue;
        }
        match crate::layout::with_field(&layout, &name, *offset) {
            Ok(changed) => {
                layout = changed;
                notes.push(format!("{:<34} {:#X} -> {:#X} (its class's code: {})", name, current.unwrap_or(0), offset, how));
                settled.insert(name.clone());
            }
            Err(e) => notes.push(format!("{:<34} couldn't be set: {:#}", name, e)),
        }
    }
    layout
}

/// For a new game: the newest dump in the data folder of each known game (of the same bitness, if there are any)
fn other_games(image: &PeImage) -> Vec<(PeImage, ProcessDetails)> {
    let mut dumps: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(crate::snapshot::dump_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dumps.sort();
    let mut newest: BTreeMap<String, (PeImage, ProcessDetails)> = BTreeMap::new();
    for (_, path) in dumps.iter().rev() {
        let dump = match PeImage::load(path) {
            Ok(dump) => dump,
            Err(_) => continue,
        };
        if let Ok(details) = derive::reference_details(&dump, None) {
            newest.entry(details.name.clone()).or_insert((dump, details));
        }
    }
    let all: Vec<(PeImage, ProcessDetails)> = newest.into_values().collect();
    if all.iter().any(|(dump, _)| dump.is_64 == image.is_64) {
        all.into_iter().filter(|(dump, _)| dump.is_64 == image.is_64).collect()
    } else {
        all
    }
}

/// A game with no entry in the offsets file yet, to start one for
pub struct NewGame {
    pub executable: String,
    pub name: String,
    // What to call this build, if not after the image size
    pub version: Option<String>,
}

/// The running game (a known executable, or `new_game`), its known details if this build is known, and the details to
/// start from: the known ones, or the last known build of the same executable
fn attach(
    known: &[ProcessDetails],
    new_game: Option<&NewGame>,
) -> Result<(Pid, ProcessHandle, usize, Option<ProcessDetails>, ProcessDetails)> {
    if let Some(new) = new_game {
        if !known.iter().any(|d| d.executable_name.eq_ignore_ascii_case(&new.executable)) {
            return attach_new(new);
        }
    }
    let mut executables: Vec<&str> = known.iter().map(|d| d.executable_name.as_str()).collect();
    executables.dedup();
    for executable in executables {
        let pid = match find_process::get_pid(executable) {
            Some(pid) => pid,
            None => continue,
        };
        let handle = pid.try_into_process_handle().with_context(|| format!("Could not open {}", executable))?;
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
    Err(anyhow!("None of the known games ({}) is running", known.iter().map(|d| d.executable_name.clone()).collect::<HashSet<_>>().into_iter().collect::<Vec<_>>().join(", ")))
}

/// A game tomb-helper doesn't know: an entry to start from with nothing but what its exe says (its pointer size and
/// image size) and ROTTR's layout, which the same engine's other games mostly share
fn attach_new(new: &NewGame) -> Result<(Pid, ProcessHandle, usize, Option<ProcessDetails>, ProcessDetails)> {
    let pid = find_process::get_pid(&new.executable).with_context(|| format!("{} isn't running", new.executable))?;
    let handle = pid.try_into_process_handle().with_context(|| format!("Could not open {}", new.executable))?;
    let base_address = find_process::get_base_address(pid) as usize;
    let image_size = find_process::get_image_size(handle, base_address)?;
    let arch = if PeImage::process_is_64(handle, base_address)? { Architecture::Arch64Bit } else { Architecture::Arch32Bit };
    println!("{} ({}) isn't a known game, so starting a new entry for it with ROTTR's layout", new.name, new.executable);
    let template = ProcessDetails {
        name: new.name.clone(),
        executable_name: new.executable.clone(),
        version: VersionIdentifier { version: format!("unknown build (image size {})", image_size), image_size: Some(image_size) },
        arch,
        address_offsets: Default::default(),
        unverified: Default::default(),
        layout: Default::default(),
    };
    Ok((pid, handle, base_address, None, template))
}

/// Lara's instance (the one with a PlayerComponent) and where she is
fn find_lara(handle: Handle, base_address: usize, array_rva: usize) -> Option<(usize, Position)> {
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
