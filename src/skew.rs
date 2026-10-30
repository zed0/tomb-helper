//! The games' own noclip, the cdc engine's "skew cheat", which flight mode can use (`flight_skew` in the config). Lara's
//! player handles a message (`SKEW_MESSAGE`) by turning off her gravity and collision and putting her in a state where
//! she stays wherever she's put; the same message without data turns it off, and the game restores both. Nothing in the
//! retail games sends it (the debug controls that did are compiled out), so it's sent by calling the game's
//! send-message function (`SendMessage`, with the message system `MessageSystem` points at) in a thread of its own
//! (`game_call`), which needs the version to have both offsets. Found and tried in SOTTR Steam 492, and found the same
//! way in the other builds of all three games (see CLAUDE.md).

use crate::components::Components;
use crate::find_process::Connection;
use crate::game::Game;
use crate::game_call::GameCall;
use crate::game_memory::{read_pointer, read_u32};
use crate::process_details::AddressType;
use crate::rtti::PLAYER;
use crate::targets;

/// The message, handled by Lara's player: with data it turns skew on, without it off
pub const SKEW_MESSAGE: u32 = 0xA09458BC;

pub struct Skew {
    call: GameCall,
}

impl Skew {
    /// For a version that has the offsets, or None
    pub fn find(connection: &Connection) -> Option<Skew> {
        let offsets = &connection.details.address_offsets;
        if !offsets.contains_key(&AddressType::SendMessage) || !offsets.contains_key(&AddressType::MessageSystem) {
            println!("Flight mode can't use skew: this version has no SendMessage and MessageSystem offsets");
            return None;
        }
        match GameCall::new(connection.pid, connection.details.arch) {
            Ok(call) => Some(Skew { call }),
            Err(e) => {
                eprintln!("Flight mode can't use skew: {}", e);
                None
            }
        }
    }

    /// Turns it on or off for Lara, once she and the message system are found
    pub fn set(&self, game: &Game, components: &mut Components, on: bool) -> Result<(), String> {
        let instance = targets::lara_instance(game.layout(), &game.offsets)
            .and_then(|chain| game.pointer_at(&chain))
            .ok_or("Lara's instance can't be found")?;
        // Her player object (which TR2013 finds from her instance, the others her instance from it) shows it's her
        let player = targets::player_chain(game.layout(), &game.offsets)
            .and_then(|chain| game.pointer_at(&chain))
            .ok_or("Lara's player object can't be found")?;
        if components.classes.name(game.handle, player).as_deref() != Some(PLAYER) {
            return Err("what Lara's position leads to isn't her player".to_string());
        }
        let system = game
            .resolve(AddressType::MessageSystem)
            .and_then(|address| read_pointer(game.handle, address))
            .ok_or("the message system can't be found")?;
        let function = game.resolve(AddressType::SendMessage).ok_or("the send-message function can't be found")?;
        let data = if on { self.call.data() } else { 0 };
        let args = [system as u64, instance as u64, SKEW_MESSAGE as u64, data as u64];
        self.call.call(function, args).map_err(|e| e.to_string())?;
        // Where the player's flags are known, check it took
        let layout = &game.layout().player;
        if let Some(flags) = layout.skew {
            let set = read_u32(game.handle, player + flags).map(|f| f as usize & layout.skew_mask != 0);
            if set != Some(on) {
                return Err(format!("the game didn't turn it {}", if on { "on" } else { "off" }));
            }
        }
        Ok(())
    }
}
