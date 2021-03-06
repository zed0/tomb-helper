use crate::action::Action;
use std::error::Error;

pub trait Handler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>>;
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>>;
}
