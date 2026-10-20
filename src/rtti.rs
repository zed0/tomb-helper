//! MSVC RTTI: an object's first pointer is its vtable, whose entry before the first is a CompleteObjectLocator with its
//! type descriptor, which has the class name (e.g. `.?AVGameRender@@`). 64-bit locators refer to things by RVA,
//! 32-bit ones (TR2013) by address.
//!
//! `Classes` maps vtables to class names (cached) and `all_vtables` lists every vtable in an image and its class.
//! Tested on fake images of both pointer sizes.

use crate::game_memory::{Handle, pointer_size, read_pointer, read_u32};
use process_memory::CopyAddress;
use std::collections::HashMap;
use std::convert::TryInto;

const MAX_CLASS_NAME: usize = 128;

// How locators are laid out at the game's pointer size
struct Format {
    pointer_size: usize,
    // A locator starts with this
    signature: u32,
    // u32: the type descriptor's RVA, or its address
    descriptor: usize,
    descriptor_is_rva: bool,
    // In the type descriptor, after its vtable pointer and a spare pointer
    name: usize,
}

impl Format {
    fn current() -> Format {
        Format::for_pointer_size(pointer_size())
    }

    fn for_pointer_size(pointer_size: usize) -> Format {
        if pointer_size == 4 {
            Format { pointer_size: 4, signature: 0, descriptor: 0xC, descriptor_is_rva: false, name: 0x8 }
        } else {
            Format { pointer_size: 8, signature: 1, descriptor: 0xC, descriptor_is_rva: true, name: 0x10 }
        }
    }

    fn descriptor_address(&self, field: usize, base_address: usize) -> usize {
        if self.descriptor_is_rva {
            base_address.wrapping_add(field)
        } else {
            field
        }
    }
}

/// RTTI class names of the game's objects, cached by vtable
pub struct Classes {
    base_address: usize,
    names: HashMap<usize, Option<String>>,
}

impl Classes {
    pub fn new(base_address: usize) -> Classes {
        Classes { base_address, names: HashMap::new() }
    }

    pub fn name(&mut self, handle: Handle, object: usize) -> Option<String> {
        let vtable = read_pointer(handle, object)?;
        self.vtable_name(handle, vtable)
    }

    pub fn vtable_name(&mut self, handle: Handle, vtable: usize) -> Option<String> {
        let base_address = self.base_address;
        self.names
            .entry(vtable)
            .or_insert_with(|| {
                // Garbage pointers can be anywhere, and reading from them just fails
                let format = Format::current();
                let locator = read_pointer(handle, vtable.checked_sub(format.pointer_size)?)?;
                let field = read_u32(handle, locator.wrapping_add(format.descriptor))? as usize;
                let descriptor = format.descriptor_address(field, base_address);
                let mut name = [0u8; MAX_CLASS_NAME];
                handle.copy_address(descriptor.wrapping_add(format.name), &mut name).ok()?;
                class_name(&name)
            })
            .clone()
    }
}

// The class name at the start of `bytes`, if it's one
fn class_name(bytes: &[u8]) -> Option<String> {
    let name = bytes.split(|&b| b == 0).next()?;
    name.starts_with(b".?A").then(|| String::from_utf8_lossy(name).into_owned())
}

/// Every vtable in the game's image (its data laid out as in memory, e.g. `PeImage::dump`) and its class's name.
/// A vtable starts right after a pointer to its locator. 64-bit locators also have their own RVA at +0x14, and
/// 32-bit ones the address of their class hierarchy at +0x10, which rules out most things that just look like one.
pub fn all_vtables(data: &[u8], base_address: usize) -> Vec<(usize, String)> {
    find_vtables(data, base_address, &Format::current())
}

fn find_vtables(data: &[u8], base_address: usize, format: &Format) -> Vec<(usize, String)> {
    let u32_at = |offset: usize| data.get(offset..offset + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let in_image = |address: usize| address >= base_address && address - base_address < data.len();
    let mut locators: HashMap<usize, String> = HashMap::new();
    for rva in (0..data.len().saturating_sub(0x18)).step_by(4) {
        if u32_at(rva) != Some(format.signature as usize) {
            continue;
        }
        let checked = if format.descriptor_is_rva {
            u32_at(rva + 0x14) == Some(rva)
        } else {
            u32_at(rva + 0x10).map_or(false, in_image)
        };
        let descriptor = match u32_at(rva + format.descriptor) {
            Some(field) if checked => format.descriptor_address(field, base_address).wrapping_sub(base_address),
            _ => continue,
        };
        let name_at = descriptor.wrapping_add(format.name);
        if let Some(name) = data.get(name_at..(name_at + MAX_CLASS_NAME).min(data.len())).and_then(class_name) {
            locators.insert(base_address + rva, name);
        }
    }
    let mut vtables = vec![];
    for (i, word) in data.chunks_exact(format.pointer_size).enumerate() {
        let mut value = [0u8; 8];
        value[..format.pointer_size].copy_from_slice(word);
        if let Some(name) = locators.get(&(u64::from_le_bytes(value) as usize)) {
            vtables.push((base_address + (i + 1) * format.pointer_size, name.clone()));
        }
    }
    vtables
}

#[cfg(test)]
mod tests {
    use super::*;

    // An image with one class: its type descriptor at 0x100, its locator at 0x200, and its vtable at 0x300, after a
    // pointer to the locator
    fn image(pointer_size: usize, base_address: usize) -> Vec<u8> {
        let format = Format::for_pointer_size(pointer_size);
        let mut data = vec![0u8; 0x400];
        let mut put = |offset: usize, value: usize, size: usize| {
            data[offset..offset + size].copy_from_slice(&(value as u64).to_le_bytes()[..size]);
        };
        let descriptor = if format.descriptor_is_rva { 0x100 } else { base_address + 0x100 };
        put(0x200, format.signature as usize, 4);
        put(0x200 + format.descriptor, descriptor, 4);
        if format.descriptor_is_rva {
            put(0x200 + 0x14, 0x200, 4);
        } else {
            put(0x200 + 0x10, base_address + 0x180, 4);
        }
        put(0x300 - pointer_size, base_address + 0x200, pointer_size);
        let name = b".?AVGameRender@@\0";
        data[0x100 + format.name..0x100 + format.name + name.len()].copy_from_slice(name);
        data
    }

    #[test]
    fn vtables_are_found_in_64_and_32_bit_images() {
        for (pointer_size, base_address) in [(8, 0x1_4000_0000), (4, 0x40_0000)] {
            let found = find_vtables(&image(pointer_size, base_address), base_address, &Format::for_pointer_size(pointer_size));
            assert_eq!(found, vec![(base_address + 0x300, ".?AVGameRender@@".to_string())], "{}-bit", pointer_size * 8);
        }
    }

    #[test]
    fn locators_of_the_other_format_are_not_found() {
        let base_address = 0x40_0000;
        assert!(find_vtables(&image(4, base_address), base_address, &Format::for_pointer_size(8)).is_empty());
    }
}
