//! The level select hotkey (`LevelSelect`): from the main menu, asks the game for the developers' level select (see
//! `menu`), whose start points load the levels. Only for a version with `MenuState`, and only written once the menus
//! and the requested menu's chain check out (`targets::find`).

use crate::action::Action;
use crate::components::Components;
use crate::find_process::Connection;
use crate::game::Game;
use crate::handler::Handler;
use crate::menu::{Menus, LEVEL_SELECT, MAIN_MENU, NO_MENU};
use crate::process_details::AddressType;
use crate::targets;
use crate::tracked_memory::TrackedMemory;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;

pub struct MenuHandler {
    game: Game,
    components: Components,
    arch: Architecture,
    handle: ProcessHandle,
}

impl MenuHandler {
    pub fn new(connection: &Connection) -> Option<MenuHandler> {
        connection.details.address_offsets.get(&AddressType::MenuState)?;
        println!("Loading menu handler...");
        Some(MenuHandler {
            game: connection.game(),
            components: Components::new(connection.base_address),
            arch: connection.details.arch,
            handle: connection.process,
        })
    }

    fn level_select(&mut self) -> Result<(), Box<dyn Error>> {
        let menus = Menus::read(&self.game, &mut self.components.classes)
            .map_err(|why| format!("Can't open the level select, as the menus can't be found: {}", why))?;
        let level_select = menus.id(LEVEL_SELECT).ok_or("This game has no level select")?;
        if menus.current == level_select {
            return Ok(());
        }
        // Elsewhere its page doesn't show, and the game's input does nothing until another menu's requested
        if Some(menus.current) != menus.id(MAIN_MENU) {
            let showing = menus.name(menus.current);
            return Err(format!("The level select only opens from the main menu (this is {})", showing).into());
        }
        if menus.requested != NO_MENU {
            return Err(format!("The game is already switching to {}", menus.name(menus.requested)).into());
        }
        let (chain, _) = targets::find(&self.game, &mut self.components, AddressType::RequestedMenu)
            .map_err(|why| format!("Can't open the level select: RequestedMenu: {}", why))?;
        let base = self.game.base_address;
        match self.game.layout().menu.id_size {
            // TR2013's are i16s, with something else after them
            2 => TrackedMemory::new(level_select as i16, chain, self.arch, base).apply_to_game(self.handle)?,
            _ => TrackedMemory::new(level_select, chain, self.arch, base).apply_to_game(self.handle)?,
        }
        println!("Opening the level select");
        Ok(())
    }
}

impl Handler for MenuHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::LevelSelect {} => self.level_select(),
            _ => Ok(()),
        }
    }
}
