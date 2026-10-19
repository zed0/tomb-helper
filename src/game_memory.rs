//! Reading the game's memory: its pointer size (8 bytes, or 4 for 32-bit TR2013), pointers and values read from it, and
//! little endian values at an offset in bytes copied out of it. Everything reading game pointers goes through these.

use nalgebra::Vector3;
use process_memory::{Architecture, CopyAddress, ProcessHandle};
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

/// The pointer at `address`, None if it can't be read or is null
pub fn read_pointer(handle: ProcessHandle, address: usize) -> Option<usize> {
    read_pointer_or_null(handle, address).ok().filter(|&pointer| pointer != 0)
}

/// The pointer at `address`, which may be null
pub fn read_pointer_or_null(handle: ProcessHandle, address: usize) -> io::Result<usize> {
    let mut bytes = vec![0u8; pointer_size()];
    handle.copy_address(address, &mut bytes)?;
    Ok(pointer_from(&bytes))
}

pub fn read_u32(handle: ProcessHandle, address: usize) -> Option<u32> {
    let mut bytes = [0u8; 4];
    handle.copy_address(address, &mut bytes).ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// `N` floats
pub fn read_floats<const N: usize>(handle: ProcessHandle, address: usize) -> Option<[f32; N]> {
    let mut bytes = vec![0u8; N * 4];
    handle.copy_address(address, &mut bytes).ok()?;
    Some(floats_at(&bytes, 0))
}

pub fn read_u64(handle: ProcessHandle, address: usize) -> Option<u64> {
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
