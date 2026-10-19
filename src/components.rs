//! Finding an instance's components by their RTTI class names, which are the same between game versions (and in
//! SOTTR), rather than by slot number. Each component type has a fixed slot in every instance's component table,
//! which is learnt by looking through a table for a component of that class (one whose back pointer is the instance;
//! many tables are shorter than the slots used, so a slot can hold something else).

use crate::layout;
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::{HashMap, HashSet};

pub const TRANSFORM: &str = ".?AVTransformComponent@cdc@@";
pub const DRAWABLE: &str = ".?AVDrawableComponent@cdc@@";
pub const COLLIDE: &str = ".?AVCollideComponent@cdc@@";
pub const PROXY: &str = ".?AVProxyComponent@cdc@@";
pub const PLAYER: &str = ".?AVPlayerComponent@@";
pub const TRIGGER: &str = ".?AVTriggerComponent@@";
// The trigger's shape (other instances have them too)
pub const SHAPE: &str = ".?AVShapeComponent@cdc@@";
// Campfires, search markers, tethers, throwables, resources to gather, NPCs to loot, ...
pub const INTERACTABLE: &str = ".?AVInteractableComponent@@";
// Pickups, documents, relics, caches
pub const LOOT: &str = ".?AVLootComponent@@";
// Documents, relics, cave markers
pub const COLLECTIBLE: &str = ".?AVCollectibleComponent@@";
// Characters and animals with AI (people, hares, squirrels, chickens, ...)
pub const NPC: &str = ".?AVNpcComponent@@";
// Birds have no NpcComponent, but like the rest can be hurt and leave a corpse (Lara has no corpse)
pub const HEALTH: &str = ".?AVHealthComponent@cdc@@";
pub const CORPSE: &str = ".?AVCorpseComponent@@";

// MSVC RTTI: an object's vtable's entry before the first is a CompleteObjectLocator, which has the RVA of a type
// descriptor with the class name
const LOCATOR_TYPE_DESCRIPTOR: usize = 0xC; // u32 RVA
const TYPE_DESCRIPTOR_NAME: usize = 0x10;
const MAX_CLASS_NAME: usize = 96;

// How much of a table to look through when learning slots (ROTTR uses up to 112), and how much to read at once
// (reads past the end of a table can fail)
const MAX_SLOTS: usize = 192;
const SLOTS_PER_READ: usize = 16;
// Tables to look through per `start_refresh` while some classes' slots are unknown
const SCANS_PER_REFRESH: usize = 48;
// Tables already looked through are remembered (so each refresh looks at new ones) up to this many
const MAX_SCANNED: usize = 100_000;

/// RTTI class names of objects, cached by vtable
pub struct Classes {
    base_address: usize,
    names: HashMap<usize, Option<String>>,
}

impl Classes {
    pub fn new(base_address: usize) -> Classes {
        Classes { base_address, names: HashMap::new() }
    }

    pub fn name(&mut self, handle: ProcessHandle, object: usize) -> Option<String> {
        let vtable = read_usize(handle, object)?;
        self.vtable_name(handle, vtable)
    }

    pub fn vtable_name(&mut self, handle: ProcessHandle, vtable: usize) -> Option<String> {
        let base_address = self.base_address;
        self.names
            .entry(vtable)
            .or_insert_with(|| {
                // Garbage pointers can be anywhere, and reading from them just fails
                let locator = read_usize(handle, vtable.checked_sub(8)?)?;
                let mut rva = [0u8; 4];
                handle.copy_address(locator.wrapping_add(LOCATOR_TYPE_DESCRIPTOR), &mut rva).ok()?;
                let mut name = [0u8; MAX_CLASS_NAME];
                let descriptor = base_address.wrapping_add(u32::from_le_bytes(rva) as usize);
                handle.copy_address(descriptor.wrapping_add(TYPE_DESCRIPTOR_NAME), &mut name).ok()?;
                let end = name.iter().position(|&c| c == 0)?;
                let name = String::from_utf8_lossy(&name[..end]).into_owned();
                Some(name).filter(|n| n.starts_with(".?A"))
            })
            .clone()
    }
}

pub struct Components {
    pub classes: Classes,
    // Learnt slot of each component class
    slots: HashMap<String, usize>,
    scans_left: usize,
    // Tables already looked through, which aren't looked through again
    scanned: HashSet<usize>,
}

impl Components {
    pub fn new(base_address: usize) -> Components {
        Components {
            classes: Classes::new(base_address),
            slots: HashMap::new(),
            scans_left: SCANS_PER_REFRESH,
            scanned: HashSet::new(),
        }
    }

    /// Allows another few tables to be looked through for slots that aren't known yet
    pub fn start_refresh(&mut self) {
        self.allow_scans(SCANS_PER_REFRESH);
    }

    /// Allows this many more tables to be looked through for slots that aren't known yet
    pub fn allow_scans(&mut self, scans: usize) {
        self.scans_left = scans;
        if self.scanned.len() > MAX_SCANNED {
            self.scanned.clear();
        }
    }

    /// The slots learnt so far, by class
    pub fn known_slots(&self) -> &HashMap<String, usize> {
        &self.slots
    }

    /// The instance's component of the class, if it has one
    pub fn get(&mut self, handle: ProcessHandle, instance: usize, class: &str) -> Option<usize> {
        let table = read_usize(handle, instance.wrapping_add(layout::current().instance.components?))?;
        if !self.slots.contains_key(class) {
            if self.scans_left == 0 || !self.scanned.insert(table) {
                return None;
            }
            self.scans_left -= 1;
            self.learn(handle, instance, table);
        }
        let slot = *self.slots.get(class)?;
        let component = read_usize(handle, table.wrapping_add(slot * layout::pointer_size()))?;
        let back = read_usize(handle, component.wrapping_add(layout::current().component.instance))?;
        if back != instance || self.classes.name(handle, component).as_deref() != Some(class) {
            return None;
        }
        Some(component)
    }

    /// Learns the slot of every component in the table
    fn learn(&mut self, handle: ProcessHandle, instance: usize, table: usize) {
        let back_offset = layout::current().component.instance;
        let size = layout::pointer_size();
        for first in (0..MAX_SLOTS).step_by(SLOTS_PER_READ) {
            let mut bytes = vec![0u8; SLOTS_PER_READ * size];
            if handle.copy_address(table.wrapping_add(first * size), &mut bytes).is_err() {
                return;
            }
            for (i, pointer) in bytes.chunks_exact(size).enumerate() {
                let component = layout::pointer_from(pointer);
                if component == 0 || read_usize(handle, component.wrapping_add(back_offset)) != Some(instance) {
                    continue;
                }
                if let Some(name) = self.classes.name(handle, component) {
                    self.slots.entry(name).or_insert(first + i);
                }
            }
        }
    }
}

// A pointer, at the game's pointer size, None if it's null
fn read_usize(handle: ProcessHandle, address: usize) -> Option<usize> {
    let mut bytes = vec![0u8; layout::pointer_size()];
    handle.copy_address(address, &mut bytes).ok()?;
    match layout::pointer_from(&bytes) {
        0 => None,
        value => Some(value),
    }
}
