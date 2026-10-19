use crate::action::Action;
use crate::position::Position;
use std::error::Error;

// What handlers have to show in the overlay's window
#[derive(Debug, Clone, Default)]
pub struct HelperState {
    // There's a position handler for this game
    pub position_tools: bool,
    pub saved_position: Option<Position>,
    // Lara's position is held by the position handler and moved by the movement hotkeys
    pub flying: bool,
}

pub trait Handler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>>;
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>>;
    // Actions asked for some other way than a hotkey (the overlay's buttons), which every handler is given
    fn take_actions(&mut self) -> Vec<Action> {
        vec![]
    }
    // Adds what it has to show to `state`
    fn report(&self, _state: &mut HelperState) {}
    // Shows everything's state (the overlay)
    fn show(&mut self, _state: &HelperState) {}
}
