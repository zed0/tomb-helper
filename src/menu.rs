//! The game's menus: `UiMenuStateManager` is a state machine whose states are the menus (`layout.menu`), each with an
//! id and a name in a table in it (e.g. "dtp::MENU_STATE_MAIN_MENU"). The ids differ between games (the level select
//! is 0x19 in ROTTR, 0x13 in SOTTR), so menus are found by name. On each frame the game switches to the requested menu
//! (`RequestedMenu`) if it isn't the one showing, leaving the one it's in as it would itself.
//!
//! The developers' level select (`LEVEL_SELECT`) is still in the games: the main menu's "Select level" button is
//! always hidden, but requested from the main menu it shows, and a start point picked in it loads (SOTTR 234.1).
//! Requested from the pause menu its page doesn't show, and the game's input does nothing until another menu is
//! requested.

use crate::game::Game;
use crate::game_memory::{read_bytes, read_u32, Handle};
use crate::layout::Layout;
use crate::process_details::{AddressOffsets, AddressType};
use crate::rtti::{Classes, MENU_STATE_MANAGER};
use process_memory::CopyAddress;

pub const MAIN_MENU: &str = "dtp::MENU_STATE_MAIN_MENU";
pub const LEVEL_SELECT: &str = "dtp::MENU_STATE_LEVEL_SELECT";
// Every menu's name starts with this
const NAME_PREFIX: &str = "dtp::MENU_STATE_";
// The names of ids with no menu (SOTTR and ROTTR, TR2013)
const UNUSED: [&str; 2] = ["static_cast<dtp::MENU_STATE>(counter)", "counter"];
/// No menu: what's showing in game, and what's requested when nothing is
pub const NO_MENU: i32 = -1;
// What else can be requested: -3 closes the menus, -4 is another way out (from the code that switches)
const SPECIAL_REQUESTS: std::ops::RangeInclusive<i32> = -4..=-1;

/// The menus' state machine as it is now
pub struct Menus {
    // By id, `UNUSED` where there's no menu
    names: Vec<String>,
    pub current: i32,
    pub requested: i32,
}

impl Menus {
    /// The menus through `MenuState`'s chain, or why they can't be read
    pub fn read(game: &Game, classes: &mut Classes) -> Result<Menus, String> {
        let chain = manager_chain(game.layout(), &game.offsets).ok_or("this version has no way to find the menus")?;
        let manager = game.pointer_at(&chain).ok_or("MenuState's chain doesn't resolve")?;
        Menus::at(game.handle, classes, manager)
    }

    /// The menus of the manager at `manager`, if it is one and its names are menus'
    pub fn at(handle: Handle, classes: &mut Classes, manager: usize) -> Result<Menus, String> {
        match classes.name(handle, manager) {
            Some(class) if class == MENU_STATE_MANAGER => {}
            Some(class) => return Err(format!("it's {}, not a UiMenuStateManager", class)),
            None => return Err("it isn't a UiMenuStateManager".to_string()),
        }
        let layout = &handle.layout().menu;
        let mut table = vec![0u8; layout.count * layout.name_size];
        handle.copy_address(manager + layout.names, &mut table).map_err(|_| "its menus' names can't be read")?;
        // The table ends where the names stop being about menus (ids without one are named `UNUSED`)
        let names: Vec<String> = table
            .chunks(layout.name_size)
            .map(|name| {
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                String::from_utf8_lossy(&name[..end]).into_owned()
            })
            .take_while(|name| name.starts_with(NAME_PREFIX) || UNUSED.contains(&name.as_str()))
            .collect();
        if !names.iter().any(|n| n == MAIN_MENU) {
            return Err("its names aren't menus'".to_string());
        }
        let read = |offset| read_id(handle, manager + offset).ok_or("its state can't be read");
        let menus = Menus { names, current: read(layout.current)?, requested: read(layout.requested)? };
        if !menus.is_menu(menus.current) && menus.current != NO_MENU {
            return Err(format!("the menu showing ({}) isn't one", menus.current));
        }
        if !menus.is_menu(menus.requested) && !SPECIAL_REQUESTS.contains(&menus.requested) {
            return Err(format!("the menu requested ({}) isn't one", menus.requested));
        }
        Ok(menus)
    }

    /// The id of the menu with this name
    pub fn id(&self, name: &str) -> Option<i32> {
        self.names.iter().position(|n| n == name).map(|id| id as i32)
    }

    /// A menu's name without the prefix every one has (e.g. "MAIN_MENU"), or its id if it has none
    pub fn name(&self, id: i32) -> String {
        match usize::try_from(id).ok().and_then(|id| self.names.get(id)).filter(|name| name.starts_with(NAME_PREFIX)) {
            Some(name) => name.trim_start_matches(NAME_PREFIX).to_string(),
            None if id == NO_MENU => "none".to_string(),
            None => id.to_string(),
        }
    }

    fn is_menu(&self, id: i32) -> bool {
        usize::try_from(id).ok().and_then(|id| self.names.get(id)).is_some_and(|name| name.starts_with(NAME_PREFIX))
    }
}

/// A menu id (`menu.id_size` bytes, 2 in TR2013), None if it can't be read
fn read_id(handle: Handle, address: usize) -> Option<i32> {
    match handle.layout().menu.id_size {
        2 => read_bytes(handle, address).map(|bytes| i16::from_le_bytes(bytes) as i32),
        _ => read_u32(handle, address).map(|id| id as i32),
    }
}

/// The chain to the pointer to the manager: `MenuState`'s less its last offset (`menu.current`)
pub fn manager_chain(layout: &Layout, offsets: &AddressOffsets) -> Option<Vec<usize>> {
    match offsets.get(&AddressType::MenuState)?.split_last() {
        Some((&current, prefix)) if !prefix.is_empty() && current == layout.menu.current => Some(prefix.to_vec()),
        _ => None,
    }
}
