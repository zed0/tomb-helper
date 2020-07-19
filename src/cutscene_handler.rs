use std::fmt;
use std::error::Error;
use process_memory::{ProcessHandle, Architecture};
use crate::process_details::{AddressType, AddressOffsets};
use crate::tracked_memory::TrackedMemory;
use crate::action::Action;
use crate::handler::Handler;

#[derive(Debug, Clone)]
pub struct CutsceneHandler {
    status: TrackedMemory<u8>,
    timeline: TrackedMemory<f32>,
    handle: ProcessHandle,
}

impl CutsceneHandler {
    pub fn new(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
    ) -> Option<CutsceneHandler> {
        Some(CutsceneHandler {
            status: TrackedMemory::<u8>::new(
                0,
                address_offsets.get(&AddressType::CutsceneStatus)?.clone(),
                *arch,
                *base_addr,
            ),
            timeline: TrackedMemory::<f32>::new(
                0.0,
                address_offsets.get(&AddressType::CutsceneTimeline)?.clone(),
                *arch,
                *base_addr,
            ),
            handle: *handle,
        })
    }

    fn skip(&mut self) -> Result<(), Box<dyn Error>> {
        if self.timeline.fetch_from_game(self.handle).is_err() {
            return Err(NotInCutsceneError.into());
        }
        if self.timeline.data <= 6.0 {
            return Err(TooEarlyInCutsceneError.into());
        }

        self.status.data = 5;
        if self.status.apply_to_game(self.handle).is_err() {
            return Err(NotInCutsceneError.into());
        }
        println!("Skipped cutscene");
        Ok(())
    }
}

impl Handler for CutsceneHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::SkipCutscene{} => self.skip(),
            _ => Ok(()),
        }
    }
}


#[derive(Debug)]
struct NotInCutsceneError;

impl fmt::Display for NotInCutsceneError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Not in a cutscene")
    }
}

impl Error for NotInCutsceneError {}


#[derive(Debug)]
struct TooEarlyInCutsceneError;

impl fmt::Display for TooEarlyInCutsceneError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Too early in cutscene")
    }
}

impl Error for TooEarlyInCutsceneError {}
