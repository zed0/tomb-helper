use std::io;
use process_memory::{DataMember, Memory, ProcessHandle, Architecture};

#[derive(Debug, Clone)]
pub struct TrackedMemory<T: Copy> {
    pub data: T,
    offsets: Vec<usize>,
    arch: Architecture,
    base_addr: usize,
}

impl<T: Copy + std::fmt::Debug> TrackedMemory<T> {
    pub fn new(
        data: T,
        offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedMemory<T> {
        TrackedMemory {
            data,
            offsets,
            arch,
            base_addr,
        }
    }

    pub fn offsets_with_base(&self) -> Vec<usize> {
        let mut offsets_with_base = self.offsets.clone();
        offsets_with_base[0] += self.base_addr;
        offsets_with_base
    }

    pub fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        self.data = DataMember::<T>::new_offset(handle, self.offsets_with_base())
            .set_arch(self.arch)
            .read()?;
        Ok(())
    }

    pub fn apply_to_game(&self, handle: ProcessHandle) -> io::Result<()> {
        DataMember::<T>::new_offset(handle, self.offsets_with_base())
            .set_arch(self.arch)
            .write(&self.data)?;
        Ok(())
    }
}

