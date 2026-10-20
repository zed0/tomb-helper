//! Reading the game's memory: its pointer size (8 bytes, or 4 for 32-bit TR2013), pointers and values read from it, and
//! little endian values at an offset in bytes copied out of it. Everything reading game pointers goes through these.

use crate::pe_image::PeImage;
use crate::snapshot::{Recording, Snapshot};
use nalgebra::Vector3;
use process_memory::{Architecture, CopyAddress, Pid, ProcessHandle};
use std::convert::TryInto;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

static POINTER_SIZE: AtomicUsize = AtomicUsize::new(8);

/// How big a game of this architecture's pointers are
pub fn pointer_size_of(arch: Architecture) -> usize {
    if matches!(arch, Architecture::Arch64Bit) {
        8
    } else {
        4
    }
}

/// Reads the game's pointers at this architecture's size from now on (`layout::set_for` does this on connecting)
pub fn set_architecture(arch: Architecture) {
    POINTER_SIZE.store(pointer_size_of(arch), Ordering::Relaxed);
}

/// How big the game's pointers are
pub fn pointer_size() -> usize {
    POINTER_SIZE.load(Ordering::Relaxed)
}

/// The pointer at the start of `bytes`, at the game's pointer size
pub fn pointer_from(bytes: &[u8]) -> usize {
    let mut value = [0u8; 8];
    let size = pointer_size().min(bytes.len());
    value[..size].copy_from_slice(&bytes[..size]);
    u64::from_le_bytes(value) as usize
}

/// Where a pointer chain from the offsets file leads (`TrackedMemory`'s way): the first offset is from the module's
/// base, each later one is added to the pointer read where the chain has got to
pub fn resolve(handle: impl CopyAddress + Copy, base_address: usize, offsets: &[usize]) -> Option<usize> {
    let (first, rest) = offsets.split_first()?;
    let mut address = base_address + first;
    for offset in rest {
        address = read_pointer(handle, address)? + offset;
    }
    Some(address)
}

/// The pointer at `address`, None if it can't be read or is null
pub fn read_pointer(handle: impl CopyAddress, address: usize) -> Option<usize> {
    read_pointer_or_null(handle, address).ok().filter(|&pointer| pointer != 0)
}

/// The pointer at `address`, which may be null
pub fn read_pointer_or_null(handle: impl CopyAddress, address: usize) -> io::Result<usize> {
    let mut bytes = vec![0u8; pointer_size()];
    handle.copy_address(address, &mut bytes)?;
    Ok(pointer_from(&bytes))
}

pub fn read_u32(handle: impl CopyAddress, address: usize) -> Option<u32> {
    let mut bytes = [0u8; 4];
    handle.copy_address(address, &mut bytes).ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// `N` floats
pub fn read_floats<const N: usize>(handle: impl CopyAddress, address: usize) -> Option<[f32; N]> {
    let mut bytes = vec![0u8; N * 4];
    handle.copy_address(address, &mut bytes).ok()?;
    Some(floats_at(&bytes, 0))
}

pub fn read_u64(handle: impl CopyAddress, address: usize) -> Option<u64> {
    let mut bytes = [0u8; 8];
    handle.copy_address(address, &mut bytes).ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// A pointer at the game's pointer size
pub fn pointer_at(bytes: &[u8], offset: usize) -> usize {
    pointer_from(&bytes[offset..])
}

pub fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

pub fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// `N` floats
pub fn floats_at<const N: usize>(bytes: &[u8], offset: usize) -> [f32; N] {
    let mut floats = [0f32; N];
    for (i, f) in floats.iter_mut().enumerate() {
        *f = f32_at(bytes, offset + i * 4);
    }
    floats
}

/// Three floats
pub fn vector_at(bytes: &[u8], offset: usize) -> Vector3<f32> {
    Vector3::new(f32_at(bytes, offset), f32_at(bytes, offset + 4), f32_at(bytes, offset + 8))
}

/// What the game's memory is read through: the running game, the running game while keeping what's read (for a
/// snapshot), or a snapshot of it. The code reading the game's structures takes this, so that it can be tested against
/// snapshots.
#[derive(Clone, Copy)]
pub enum Handle {
    Live(Pid, ProcessHandle),
    Recording(Pid, ProcessHandle, &'static Recording),
    Snapshot(&'static Snapshot),
}

// Process handles can be used from any thread; the rest is immutable or locked
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl CopyAddress for Handle {
    fn copy_address(&self, address: usize, buffer: &mut [u8]) -> io::Result<()> {
        match self {
            Handle::Live(_, handle) => handle.copy_address(address, buffer),
            Handle::Recording(_, handle, recording) => {
                handle.copy_address(address, buffer)?;
                recording.add(address, buffer);
                Ok(())
            }
            Handle::Snapshot(snapshot) => snapshot.copy_address(address, buffer),
        }
    }
}

impl Handle {
    /// A read that isn't kept in a recording, for scans through all of the game's memory (`regions`)
    pub fn scan_read(&self, address: usize, buffer: &mut [u8]) -> io::Result<()> {
        match self {
            Handle::Live(_, handle) | Handle::Recording(_, handle, _) => handle.copy_address(address, buffer),
            Handle::Snapshot(snapshot) => snapshot.copy_address(address, buffer),
        }
    }

    /// The game's committed, readable, private memory (its heaps), as (start, size); for a snapshot, what it has
    pub fn regions(&self) -> Vec<(usize, usize)> {
        match self {
            Handle::Live(_, handle) | Handle::Recording(_, handle, _) => private_regions(*handle),
            Handle::Snapshot(snapshot) => snapshot.regions(),
        }
    }

    /// The game's main module as it is in memory
    pub fn image(&self) -> io::Result<PeImage> {
        match self {
            Handle::Live(pid, _) => PeImage::dump(*pid),
            Handle::Recording(pid, _, recording) => recording.image(*pid),
            Handle::Snapshot(snapshot) => Ok(snapshot.image.clone()),
        }
    }
}

#[cfg(windows)]
fn private_regions(handle: ProcessHandle) -> Vec<(usize, usize)> {
    use winapi::um::memoryapi::VirtualQueryEx;
    use winapi::um::winnt::{
        MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_PRIVATE, PAGE_EXECUTE_READWRITE, PAGE_READONLY, PAGE_READWRITE,
    };
    let mut regions = vec![];
    let mut address = 0usize;
    loop {
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let written =
            unsafe { VirtualQueryEx(handle, address as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if written == 0 {
            break;
        }
        // Plain pages only: write combined or uncached ones (graphics memory) are very slow to read, and don't have
        // the game's objects in them
        let readable = [PAGE_READWRITE, PAGE_READONLY, PAGE_EXECUTE_READWRITE].contains(&info.Protect);
        if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && readable {
            regions.push((info.BaseAddress as usize, info.RegionSize));
        }
        match (info.BaseAddress as usize).checked_add(info.RegionSize) {
            Some(next) if next > address => address = next,
            _ => break,
        }
    }
    regions
}

#[cfg(not(windows))]
fn private_regions(_handle: ProcessHandle) -> Vec<(usize, usize)> {
    vec![]
}
