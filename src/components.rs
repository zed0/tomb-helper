//! Finding an instance's components by their RTTI class names, which are the same between game versions (and in
//! SOTTR), rather than by slot number. Each component type has a fixed slot in every instance's component table,
//! which is learnt by looking through a table for a component of that class (one whose back pointer is the instance;
//! many tables are shorter than the slots used, so a slot can hold something else).
//!
//! Learning slots is budgeted per `start_refresh`, so a single refresh may not know every class yet.

use crate::rtti::Classes;
use crate::game_memory::{Handle, pointer_from, pointer_size, read_pointer};
use crate::layout;
use process_memory::CopyAddress;
use std::collections::{HashMap, HashSet};

pub const TRANSFORM: &str = ".?AVTransformComponent@cdc@@";
pub const DRAWABLE: &str = ".?AVDrawableComponent@cdc@@";
pub const COLLIDE: &str = ".?AVCollideComponent@cdc@@";
pub const PROXY: &str = ".?AVProxyComponent@cdc@@";
pub const PLAYER: &str = ".?AVPlayerComponent@@";
// Lara's velocity
pub const BALLISTIC: &str = ".?AVBallisticComponent@cdc@@";
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
// TR2013's triggers are game objects of these classes (`layout.instance.game_object`)
pub const TRIGGER_VOLUME: &str = ".?AVNsTriggerVolumeBase@@";
pub const TRIGGER_PLANE: &str = ".?AVNsTriggerPlaneBase@@";

// How much of a table to look through when learning slots (ROTTR uses up to 112), and how much to read at once
// (reads past the end of a table can fail)
const MAX_SLOTS: usize = 192;
const SLOTS_PER_READ: usize = 16;
// Tables to look through per `start_refresh` while some classes' slots are unknown
const SCANS_PER_REFRESH: usize = 48;
// Tables already looked through are remembered (so each refresh looks at new ones) up to this many
const MAX_SCANNED: usize = 100_000;

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
    pub fn get(&mut self, handle: Handle, instance: usize, class: &str) -> Option<usize> {
        let table = Table::of(handle, instance)?;
        if !self.slots.contains_key(class) {
            if self.scans_left == 0 || !self.scanned.insert(table.address) {
                return None;
            }
            self.scans_left -= 1;
            self.learn(handle, instance, &table);
        }
        let slot = *self.slots.get(class)?;
        if slot >= table.slots {
            return None;
        }
        let component = read_pointer(handle, table.address.wrapping_add(slot * pointer_size()))?;
        if !table.is_back_pointer(handle, component, instance) || self.classes.name(handle, component).as_deref() != Some(class) {
            return None;
        }
        Some(component)
    }

    /// Learns the slot of every component in the table
    fn learn(&mut self, handle: Handle, instance: usize, table: &Table) {
        let size = pointer_size();
        for first in (0..table.slots).step_by(SLOTS_PER_READ) {
            let count = SLOTS_PER_READ.min(table.slots - first);
            let mut bytes = vec![0u8; count * size];
            if handle.copy_address(table.address.wrapping_add(first * size), &mut bytes).is_err() {
                return;
            }
            for (i, pointer) in bytes.chunks_exact(size).enumerate() {
                let component = pointer_from(pointer);
                if component == 0 || !table.is_back_pointer(handle, component, instance) {
                    continue;
                }
                if let Some(name) = self.classes.name(handle, component) {
                    self.slots.entry(name).or_insert(first + i);
                }
            }
        }
    }
}

/// Where an instance's component slots are: a table it points at, or (where there's none) one in the instance itself
pub struct Table {
    pub address: usize,
    pub slots: usize,
    inline: bool,
}

impl Table {
    pub fn of(handle: Handle, instance: usize) -> Option<Table> {
        let layout = &layout::current().instance;
        if let Some(offset) = layout.components {
            let address = read_pointer(handle, instance.wrapping_add(offset))?;
            return Some(Table { address, slots: MAX_SLOTS, inline: false });
        }
        let offset = layout.inline_components?;
        let slots = layout.size.checked_sub(offset)? / pointer_size();
        Some(Table { address: instance.wrapping_add(offset), slots, inline: true })
    }

    // Whether the component is the instance's: a table elsewhere can be shorter than the slots used, so a slot can hold
    // something else, but one in the instance holds only its own (and not all of TR2013's point back at it)
    fn is_back_pointer(&self, handle: Handle, component: usize, instance: usize) -> bool {
        self.inline || read_pointer(handle, component.wrapping_add(layout::current().component.instance)) == Some(instance)
    }
}

