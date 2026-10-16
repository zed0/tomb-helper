use process_memory::{Architecture, ProcessHandle};

use crate::position::Position;
use crate::tracked_memory::TrackedMemory;

use std::fmt;
use std::io;

#[derive(Debug, Clone)]
pub struct TrackedPosition {
    pub x: TrackedMemory<f32>,
    pub y: TrackedMemory<f32>,
    pub z: TrackedMemory<f32>,
}

impl TrackedPosition {
    pub fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.fetch_from_game(handle)?;
        self.y.fetch_from_game(handle)?;
        self.z.fetch_from_game(handle)?;
        Ok(())
    }

    pub fn apply_to_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.x.apply_to_game(handle)?;
        self.y.apply_to_game(handle)?;
        self.z.apply_to_game(handle)?;
        Ok(())
    }

    pub fn new(
        x_offsets: Vec<usize>,
        y_offsets: Vec<usize>,
        z_offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedPosition {
        TrackedPosition {
            x: TrackedMemory::new(0.0, x_offsets, arch, base_addr),
            y: TrackedMemory::new(0.0, y_offsets, arch, base_addr),
            z: TrackedMemory::new(0.0, z_offsets, arch, base_addr),
        }
    }

    pub fn to_position(&self) -> Position {
        Position {
            x: self.x.data,
            y: self.y.data,
            z: self.z.data,
        }
    }
}

impl fmt::Display for TrackedPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x.data, self.y.data, self.z.data)
    }
}
