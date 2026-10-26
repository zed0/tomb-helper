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

/// Where a component class's slot is: an index in the table the instance points at, or an offset in the instance
/// itself (the table in it, `instance.inline_components`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Table(usize),
    Inline(usize),
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Slot::Table(index) => write!(f, "{}", index),
            Slot::Inline(offset) => write!(f, "+{:#X}", offset),
        }
    }
}

pub struct Components {
    pub classes: Classes,
    // Learnt slot of each component class
    slots: HashMap<String, Slot>,
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
    pub fn known_slots(&self) -> &HashMap<String, Slot> {
        &self.slots
    }

    /// The instance's component of the class, if it has one
    pub fn get(&mut self, handle: Handle, instance: usize, class: &str) -> Option<usize> {
        if !self.slots.contains_key(class) {
            let tables = Table::all(handle, instance);
            if self.scans_left == 0 || tables.is_empty() || !self.scanned.insert(instance) {
                return None;
            }
            self.scans_left -= 1;
            for table in &tables {
                self.learn(handle, instance, table);
            }
        }
        let slot = *self.slots.get(class)?;
        let table = Table::all(handle, instance).into_iter().find(|t| t.has(slot))?;
        let component = read_pointer(handle, table.address_of(instance, slot))?;
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
                    self.slots.entry(name).or_insert(table.slot(first + i));
                }
            }
        }
    }
}

/// Where some of an instance's component slots are: a table it points at, or the one in the instance itself
pub struct Table {
    pub address: usize,
    pub slots: usize,
    // Where it starts in the instance, if it's in it
    inline: Option<usize>,
}

impl Table {
    /// The instance's tables: the one it points at first, then the one in it, where the layout has them
    pub fn all(handle: Handle, instance: usize) -> Vec<Table> {
        let layout = &layout::current().instance;
        let mut tables = vec![];
        if let Some(address) = layout.components.and_then(|offset| read_pointer(handle, instance.wrapping_add(offset))) {
            tables.push(Table { address, slots: MAX_SLOTS, inline: None });
        }
        if let Some(offset) = layout.inline_components {
            let slots = layout.size.saturating_sub(offset) / pointer_size();
            tables.push(Table { address: instance.wrapping_add(offset), slots, inline: Some(offset) });
        }
        tables
    }

    fn slot(&self, index: usize) -> Slot {
        match self.inline {
            Some(offset) => Slot::Inline(offset + index * pointer_size()),
            None => Slot::Table(index),
        }
    }

    fn has(&self, slot: Slot) -> bool {
        match (slot, self.inline) {
            (Slot::Table(index), None) => index < self.slots,
            (Slot::Inline(offset), Some(start)) => offset >= start && (offset - start) / pointer_size() < self.slots,
            _ => false,
        }
    }

    fn address_of(&self, instance: usize, slot: Slot) -> usize {
        match slot {
            Slot::Table(index) => self.address.wrapping_add(index * pointer_size()),
            Slot::Inline(offset) => instance.wrapping_add(offset),
        }
    }

    // Whether the component is the instance's: a table elsewhere can be shorter than the slots used, so a slot can hold
    // something else, but one in the instance holds only its own (and not all of TR2013's point back at it)
    fn is_back_pointer(&self, handle: Handle, component: usize, instance: usize) -> bool {
        self.inline.is_some() || read_pointer(handle, component.wrapping_add(layout::current().component.instance)) == Some(instance)
    }
}

