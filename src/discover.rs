//! `tomb-helper discover`: finds the overlay's static addresses in the running game through the RTTI class names of
//! what they point to, which are the same between versions (and in SOTTR). Uses the layout for the offsets within.

use crate::game_memory::Handle;
use crate::chains;
use crate::components;
use crate::rtti::Classes;
use crate::derive::{format_offsets, References};
use crate::layout;
use crate::offsets_file::{to_json, Hex, OffsetsFile, OffsetsSource, VersionEntry};
use crate::pe_image::PeImage;
use crate::process_details::AddressType;
use process_memory::CopyAddress;
use std::collections::{BTreeMap, HashMap};
use std::convert::TryInto;

const GAME_RENDER: &str = ".?AVGameRender@@";
// Holds the cdc::CollisionSceneClient
const MULTIBODY_CLIENT: &str = ".?AVTR9MultibodyClient@@";
const COLLISION_SCENE_CLIENT: &str = ".?AVCollisionSceneClient@cdc@@";
// How far into the multibody client to look for the collision scene client
const MULTIBODY_CLIENT_SIZE: usize = 0x200;
// The instance array is the longest run of slots with the same header, which should be at least this long
const MIN_INSTANCES: usize = 1000;
// Slots to check have TransformComponents (or are unused)
const INSTANCES_CHECKED: usize = 64;

pub fn discover(version: Option<String>, save: bool) -> Result<(), String> {
    let (pid, handle, base_address, details) = chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    println!("Dumping the game...");
    let image = PeImage::dump(pid).map_err(|e| format!("Could not dump the game: {}", e))?;
    let found = find_statics(Handle::Live(pid, handle), base_address, &image);

    println!();
    for (address_type, chain) in &found {
        let note = match details.address_offsets.get(address_type) {
            Some(known) if known == chain => "matches the offsets file",
            Some(_) => "DIFFERS from the offsets file",
            None => "new",
        };
        println!("  {:<15} {}  ({})", format!("{:?}", address_type), format_offsets(chain), note);
    }
    if save {
        let mut entry = VersionEntry::from_details(&details);
        for (address_type, chain) in &found {
            entry.offsets.insert(*address_type, chain.iter().map(|o| Hex(*o)).collect());
        }
        save_entry(entry)?;
    }
    Ok(())
}

/// The statics found through the RTTI classes of what they point to, for the game whose image this is
pub fn find_statics(handle: Handle, base_address: usize, image: &PeImage) -> BTreeMap<AddressType, Vec<usize>> {
    let references = References::new(image);
    let mut classes = Classes::new(base_address);

    // Code referenced statics holding a pointer to an object, by the object's class, with how many references
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for (target, _) in references.all() {
        *counts.entry(*target as usize).or_insert(0) += 1;
    }
    let mut by_class: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (&rva, &count) in &counts {
        let pointer = match pointer_at(&image.data, rva) {
            Some(p) if !(base_address..base_address + image.data.len()).contains(&p) => p,
            _ => continue,
        };
        if let Some(class) = classes.name(handle, pointer) {
            if class == GAME_RENDER || class == MULTIBODY_CLIENT {
                by_class.entry(class).or_default().push((rva, count));
            }
        }
    }
    let best = |class: &str| by_class.get(class).and_then(|s| s.iter().max_by_key(|(_, count)| *count).copied());

    let layout = layout::current();
    let mut found: BTreeMap<AddressType, Vec<usize>> = BTreeMap::new();
    match best(GAME_RENDER) {
        Some((rva, count)) => {
            println!("GameRender at mod+{:X} ({} code references)", rva, count);
            found.insert(AddressType::CameraView, vec![rva, layout.camera.view]);
            found.insert(AddressType::CameraLens, vec![rva, layout.camera.lens]);
            found.insert(AddressType::CameraSin, vec![rva, layout.camera.sin]);
            found.insert(AddressType::CameraCos, vec![rva, layout.camera.cos]);
        }
        None => println!("GameRender: NOT FOUND"),
    }
    match best(MULTIBODY_CLIENT) {
        Some((rva, count)) => {
            let client = pointer_at(&image.data, rva).unwrap();
            match field_of_class(handle, &mut classes, client, COLLISION_SCENE_CLIENT) {
                Some(field) => {
                    println!("TR9MultibodyClient at mod+{:X} ({} code references), scene client at +{:X}", rva, count, field);
                    found.insert(AddressType::CollisionScene, vec![rva, field, 0]);
                }
                None => println!("TR9MultibodyClient at mod+{:X}, but no CollisionSceneClient in it", rva),
            }
        }
        None => println!("TR9MultibodyClient: NOT FOUND"),
    }
    match find_instance_array(handle, &mut classes, image, base_address) {
        Some((rva, slots)) => {
            println!("Instance array at mod+{:X} ({} slots)", rva, slots);
            found.insert(AddressType::InstanceArray, vec![rva]);
        }
        None => println!("Instance array: NOT FOUND"),
    }
    found
}

// The offset in the object of a pointer to an object of the class
fn field_of_class(handle: Handle, classes: &mut Classes, object: usize, class: &str) -> Option<usize> {
    let mut bytes = vec![0u8; MULTIBODY_CLIENT_SIZE];
    handle.copy_address(object, &mut bytes).ok()?;
    (0..MULTIBODY_CLIENT_SIZE).step_by(8).find(|&offset| {
        let pointer = pointer_at(&bytes, offset).unwrap_or(0);
        pointer != 0 && classes.name(handle, pointer).as_deref() == Some(class)
    })
}

// The longest run of instance slots with the same nonzero header, if their transforms are TransformComponents
fn find_instance_array(
    handle: Handle,
    classes: &mut Classes,
    image: &PeImage,
    base_address: usize,
) -> Option<(usize, usize)> {
    let layout = &layout::current().instance;
    let data = &image.data;
    let header = |rva: usize| data.get(rva + layout.header..rva + layout.header + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut best: Option<(usize, usize)> = None;
    let mut rva = 0;
    while rva + layout.size < data.len() {
        let value = header(rva).unwrap_or(0);
        if value == 0 || header(rva + layout.size) != Some(value) || header(rva + 2 * layout.size) != Some(value) {
            rva += 8;
            continue;
        }
        let mut slots = 1;
        while header(rva + slots * layout.size) == Some(value) {
            slots += 1;
        }
        if slots >= MIN_INSTANCES && best.map_or(true, |(_, s)| slots > s) && has_transforms(handle, classes, image, base_address, rva, slots) {
            best = Some((rva, slots));
        }
        rva += 8;
    }
    best
}

fn has_transforms(
    handle: Handle,
    classes: &mut Classes,
    image: &PeImage,
    base_address: usize,
    rva: usize,
    slots: usize,
) -> bool {
    let layout = &layout::current().instance;
    // Nothing to check against if instances don't point at their transforms
    let transform = match layout.transform {
        Some(offset) => offset,
        None => return true,
    };
    let mut transforms = 0;
    for slot in (0..slots).step_by((slots / INSTANCES_CHECKED).max(1)) {
        match pointer_at(&image.data, rva + slot * layout.size + transform) {
            Some(0) | None => continue,
            Some(pointer) if (base_address..base_address + image.data.len()).contains(&pointer) => return false,
            Some(pointer) => {
                if classes.name(handle, pointer).as_deref() != Some(components::TRANSFORM) {
                    return false;
                }
                transforms += 1;
            }
        }
    }
    transforms > 0
}

fn pointer_at(bytes: &[u8], offset: usize) -> Option<usize> {
    bytes.get(offset..offset + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize)
}

/// Replaces the version's entry (the one with the same image size, or version string if there's none) in the
/// offsets file next to the exe, or adds it, starting from the built in offsets if there's no file yet
pub fn save_entry(entry: VersionEntry) -> Result<(), String> {
    let mut source = OffsetsSource::new();
    let mut versions: Vec<VersionEntry> = source.load()?.iter().map(VersionEntry::from_details).collect();
    let same = |e: &VersionEntry| {
        e.executable == entry.executable
            && match (e.image_size, entry.image_size) {
                (Some(a), Some(b)) => a == b,
                _ => e.version == entry.version,
            }
    };
    match versions.iter().position(same) {
        Some(index) => versions[index] = entry,
        None => versions.push(entry),
    }
    let file = OffsetsFile { versions };
    std::fs::write(&source.path, to_json(&file) + "\n").map_err(|e| format!("Could not write {:?}: {}", source.path, e))?;
    println!("Saved to {:?}", source.path);
    Ok(())
}
