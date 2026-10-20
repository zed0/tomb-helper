//! The game (running, or a snapshot of it) and where things are in it, from its version's offsets: how the overlay and
//! `check` both find Lara and her collision, so that `check` tests what the overlay draws.

use crate::collision::{self, ProxyCapsules};
use crate::components::{self, Components};
use crate::game_memory::{read_pointer, resolve, Handle};
use crate::process_details::{AddressOffsets, AddressType};
use crate::ui_ipc_data::{Capsule, Probe};
use std::io;

/// Lara is the nearest instance with a PlayerComponent, out of this many
pub const LARA_CANDIDATES: usize = 8;

#[derive(Clone)]
pub struct Game {
    pub handle: Handle,
    pub base_address: usize,
    pub offsets: AddressOffsets,
}

impl Game {
    pub fn new(handle: Handle, base_address: usize, offsets: &AddressOffsets) -> Game {
        Game { handle, base_address, offsets: offsets.clone() }
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
            io::Error::new(io::ErrorKind::NotFound, format!("{:?}'s pointer chain doesn't resolve", address_type))
        })
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

    /// TR2013's aim sweep while Lara has a weapon out, found from her proxy, and whether it's pushing her. None if the
    /// game doesn't have the offsets, or there's no weapon out
    pub fn aim_sweep(&self, proxy: usize) -> Option<(Capsule, bool)> {
        let instances = self.resolve(AddressType::InstanceArray)?;
        let inventories = self.resolve(AddressType::InventoryPool)?;
        let types = self.resolve(AddressType::TypePool)?;
        let lara = read_pointer(self.handle, proxy + crate::layout::current().aim_sweep.proxy_instance)?;
        crate::aim_sweep::read(self.handle, instances, inventories, types, lara)
    }

    /// Lara's proxy's shapes and those of the proxies attached to it, with the aim sweep drawn as one of them
    pub fn lara_proxies(&self, proxy: usize) -> io::Result<ProxyCapsules> {
        let mut proxies = collision::read_proxy(self.handle, proxy, &self.standalone_proxies())?;
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
