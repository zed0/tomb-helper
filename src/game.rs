//! The game (running, or a snapshot of it) and where things are in it, from its version's offsets: how the overlay and
//! `check` both find Lara and her collision, so that `check` tests what the overlay draws.
//!
//! Finding things in the game belongs here (`find_lara`, `lara_proxy`, `standalone_proxies`, `aim_sweep`,
//! `lara_proxies`, `probes`, `read_floats`, `lara_position`), rather than in the overlay and `check` separately.

use crate::collision::{self, AttachedProxy, ProxyCapsules};
use crate::components::{self, Components};
use crate::game_memory::{read_floats, read_pointer, resolve, Handle};
use crate::position::Position;
use crate::process_details::{AddressOffsets, AddressType};
use crate::ui_ipc_data::Probe;
use std::io;

/// Lara is the nearest instance with a PlayerComponent, out of this many
pub const LARA_CANDIDATES: usize = 8;

pub struct Game {
    pub handle: Handle,
    pub base_address: usize,
    pub offsets: AddressOffsets,
    // The bodies attached to Lara's, remembered between reads of her proxy
    attached_bodies: std::sync::Mutex<collision::AttachedBodies>,
}

// A copy finds the attached bodies for itself
impl Clone for Game {
    fn clone(&self) -> Game {
        Game::new(self.handle, self.base_address, &self.offsets)
    }
}

impl Game {
    pub fn new(handle: Handle, base_address: usize, offsets: &AddressOffsets) -> Game {
        Game { handle, base_address, offsets: offsets.clone(), attached_bodies: Default::default() }
    }

    /// Whether the version has offsets for this
    pub fn has(&self, address_type: AddressType) -> bool {
        self.offsets.contains_key(&address_type)
    }

    /// Where the version's offsets for this lead, None if it has none or they don't resolve
    pub fn resolve(&self, address_type: AddressType) -> Option<usize> {
        resolve(self.handle, self.base_address, self.offsets.get(&address_type)?)
    }

    /// The same, as an error if they don't resolve (or there are none)
    pub fn require(&self, address_type: AddressType) -> io::Result<usize> {
        self.resolve(address_type).ok_or_else(|| {
            crate::game_memory::unreadable(format!("{:?}'s pointer chain doesn't resolve", address_type))
        })
    }

    /// `N` floats where the offsets for this lead, read at once (so e.g. a matrix isn't a mix of two game frames)
    pub fn read_floats<const N: usize>(&self, address_type: AddressType) -> io::Result<[f32; N]> {
        read_floats(self.handle, self.require(address_type)?)
            .ok_or_else(|| crate::game_memory::unreadable(format!("{:?} couldn't be read", address_type)))
    }

    /// Lara's position, from the position offsets
    pub fn lara_position(&self) -> io::Result<Position> {
        let [x] = self.read_floats(AddressType::XPosition)?;
        let [y] = self.read_floats(AddressType::YPosition)?;
        let [z] = self.read_floats(AddressType::ZPosition)?;
        Ok(Position { x, y, z })
    }

    /// Lara's instance, out of these instances nearest her position first: the first with a PlayerComponent. None where
    /// instances have no component table (TR2013)
    pub fn find_lara(&self, components: &mut Components, nearest: &[usize]) -> Option<usize> {
        nearest
            .iter()
            .take(LARA_CANDIDATES)
            .copied()
            .find(|&address| components.get(self.handle, address, components::PLAYER).is_some())
    }

    /// Lara's movement proxy: a static (`PlayerProxy`, TR2013), else her ProxyComponent
    pub fn lara_proxy(&self, components: &mut Components, lara: Option<usize>) -> Option<usize> {
        match (self.has(AddressType::PlayerProxy), lara) {
            (true, _) => self.resolve(AddressType::PlayerProxy),
            (false, Some(lara)) => components.get(self.handle, lara, components::PROXY),
            (false, None) => None,
        }
    }

    /// Proxies attached to hers that are found on their own rather than through her proxy's body (TR2013's combat
    /// proxy, `CombatProxy`)
    pub fn standalone_proxies(&self) -> Vec<usize> {
        self.resolve(AddressType::CombatProxy).into_iter().collect()
    }

    /// The proxies attached to Lara's `proxy`: those its body's attached bodies belong to (where the layout says how to
    /// find them, TR2013), then any found on their own
    pub fn attached_proxies(&self, proxy: usize) -> Vec<usize> {
        let layout = crate::layout::current();
        let body = layout.proxy.body.and_then(|offset| read_pointer(self.handle, proxy + offset)).filter(|&b| b != 0);
        let mut proxies: Vec<usize> = match body {
            Some(body) => {
                let mut remembered = self.attached_bodies.lock().unwrap_or_else(|e| e.into_inner());
                remembered.of(self.handle, body).iter().filter_map(|&b| collision::owner_proxy(self.handle, b)).collect()
            }
            None => vec![],
        };
        for other in self.standalone_proxies() {
            if !proxies.contains(&other) {
                proxies.push(other);
            }
        }
        proxies
    }

    /// TR2013's aim sweep while Lara has a weapon out, found from her proxy, and whether it's pushing her. None if the
    /// game doesn't have the offsets, or there's no weapon out
    pub fn aim_sweep(&self, proxy: usize) -> Option<AttachedProxy> {
        let instances = self.resolve(AddressType::InstanceArray)?;
        let inventories = self.resolve(AddressType::InventoryPool)?;
        let types = self.resolve(AddressType::TypePool)?;
        let lara = read_pointer(self.handle, proxy + crate::layout::current().proxy.instance?)?;
        crate::aim_sweep::read(self.handle, instances, inventories, types, lara)
    }

    /// Lara's proxy's shapes and those of the proxies attached to it, with the aim sweep drawn as one of them
    pub fn lara_proxies(&self, proxy: usize) -> io::Result<ProxyCapsules> {
        let mut remembered = self.attached_bodies.lock().unwrap_or_else(|e| e.into_inner());
        let mut proxies = collision::read_proxy(self.handle, proxy, &self.standalone_proxies(), Some(&mut remembered))?;
        if let Some(rod) = self.aim_sweep(proxy) {
            proxies.attached.push(rod);
        }
        Ok(proxies)
    }

    /// Everyone's ground probes (`collision::probes_near` picks Lara's), None if the game has no `PlayerProbes`
    pub fn probes(&self) -> Option<io::Result<Vec<Probe>>> {
        if !self.has(AddressType::PlayerProbes) {
            return None;
        }
        Some(self.require(AddressType::PlayerProbes).and_then(|address| collision::read_probes(self.handle, address)))
    }
}
