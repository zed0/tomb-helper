use crate::find_process;
use process_memory::{CopyAddress, Pid, TryIntoProcessHandle};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// A PE image laid out as it would be in memory, so that offsets into `data` are RVAs.
#[derive(Clone)]
pub struct PeImage {
    pub data: Vec<u8>,
    pub is_64: bool,
    // Base address that absolute addresses in the image are relative to. For a dump this is the
    // address the module was actually loaded at, since the loader rewrites the header.
    pub image_base: u64,
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn u16_at(data: &[u8], offset: usize) -> io::Result<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| invalid("PE header truncated"))
}

fn u32_at(data: &[u8], offset: usize) -> io::Result<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| invalid("PE header truncated"))
}

fn u64_at(data: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u32_at(data, offset)? as u64 | (u32_at(data, offset + 4)? as u64) << 32)
}

struct Headers {
    is_64: bool,
    image_base: u64,
    size_of_image: usize,
    // (virtual address, virtual size, raw pointer, raw size)
    sections: Vec<(usize, usize, usize, usize)>,
}

fn parse_headers(data: &[u8]) -> io::Result<Headers> {
    if data.get(0..2) != Some(b"MZ") {
        return Err(invalid("Not a PE file (missing MZ)"));
    }
    let pe = u32_at(data, 0x3C)? as usize;
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(invalid("Not a PE file (missing PE signature)"));
    }
    let section_count = u16_at(data, pe + 6)? as usize;
    let optional_header_size = u16_at(data, pe + 20)? as usize;
    let optional_header = pe + 24;
    let is_64 = match u16_at(data, optional_header)? {
        0x10B => false,
        0x20B => true,
        _ => return Err(invalid("Unknown optional header magic")),
    };
    let image_base = if is_64 {
        u64_at(data, optional_header + 24)?
    } else {
        u32_at(data, optional_header + 28)? as u64
    };
    let size_of_image = u32_at(data, optional_header + 56)? as usize;

    let section_table = optional_header + optional_header_size;
    let sections = (0..section_count)
        .map(|i| {
            let s = section_table + i * 40;
            Ok((
                u32_at(data, s + 12)? as usize,
                u32_at(data, s + 8)? as usize,
                u32_at(data, s + 20)? as usize,
                u32_at(data, s + 16)? as usize,
            ))
        })
        .collect::<io::Result<_>>()?;

    Ok(Headers {
        is_64,
        image_base,
        size_of_image,
        sections,
    })
}

impl PeImage {
    /// 64 or 32, for decoding its code
    pub fn bitness(&self) -> u32 {
        if self.is_64 {
            64
        } else {
            32
        }
    }

    /// Loads either an exe from disk (mapping its sections to their RVAs) or a dump written by `dump`.
    pub fn load(path: &Path) -> io::Result<PeImage> {
        let file = fs::read(path)?;
        let headers = parse_headers(&file)?;

        // A dump is already in memory layout
        if file.len() == headers.size_of_image {
            return Ok(PeImage {
                data: file,
                is_64: headers.is_64,
                image_base: headers.image_base,
            });
        }

        let mut data = vec![0_u8; headers.size_of_image];
        let header_size = 0x1000.min(file.len());
        data[..header_size].copy_from_slice(&file[..header_size]);
        for (va, virtual_size, raw_ptr, raw_size) in headers.sections {
            let len = raw_size
                .min(virtual_size)
                .min(file.len().saturating_sub(raw_ptr))
                .min(data.len().saturating_sub(va));
            data[va..va + len].copy_from_slice(&file[raw_ptr..raw_ptr + len]);
        }

        Ok(PeImage {
            data,
            is_64: headers.is_64,
            image_base: headers.image_base,
        })
    }

    /// Copies the main module of a running process. This works for packed executables (e.g. Denuvo)
    /// since by the time the game is at the menu the code has been unpacked.
    #[cfg(windows)]
    pub fn dump(pid: Pid) -> io::Result<PeImage> {
        use winapi::um::memoryapi::VirtualQueryEx;
        use winapi::um::winnt::{MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS};

        let handle = pid.try_into_process_handle()?;
        let base_addr = find_process::get_base_address(pid) as usize;
        if base_addr == 0 {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Could not find the main module"));
        }
        let size = find_process::get_image_size(handle, base_addr)?;

        let mut data = vec![0_u8; size];
        let mut addr = base_addr;
        while addr < base_addr + size {
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            let written = unsafe {
                VirtualQueryEx(
                    handle,
                    addr as *const _,
                    &mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0 {
                return Err(io::Error::last_os_error());
            }
            let region_end = (info.BaseAddress as usize + info.RegionSize).min(base_addr + size);
            if info.State == MEM_COMMIT && info.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0 {
                // Unreadable pages are left zeroed
                let _ = handle.copy_address(addr, &mut data[addr - base_addr..region_end - base_addr]);
            }
            addr = region_end;
        }

        let headers = parse_headers(&data)?;
        Ok(PeImage {
            data,
            is_64: headers.is_64,
            image_base: headers.image_base,
        })
    }

    #[cfg(not(windows))]
    pub fn dump(_pid: Pid) -> io::Result<PeImage> {
        panic!("tomb-helper is only supported on Windows");
    }

    pub fn size_of_image(&self) -> usize {
        self.data.len()
    }

    /// The sections, from the headers at the start of the image (empty if they're missing, as in tests)
    pub fn sections(&self) -> Vec<Section> {
        let data = &self.data;
        let parse = || -> io::Result<Vec<Section>> {
            let pe = u32_at(data, 0x3C)? as usize;
            if data.get(pe..pe + 4) != Some(b"PE\0\0") {
                return Err(invalid("No PE header"));
            }
            let section_count = u16_at(data, pe + 6)? as usize;
            let section_table = pe + 24 + u16_at(data, pe + 20)? as usize;
            (0..section_count)
                .map(|i| {
                    let s = section_table + i * 40;
                    let characteristics = u32_at(data, s + 36)?;
                    Ok(Section {
                        start: u32_at(data, s + 12)? as usize,
                        end: u32_at(data, s + 12)? as usize + u32_at(data, s + 8)? as usize,
                        executable: characteristics & SECTION_EXECUTE != 0,
                        writable: characteristics & SECTION_WRITE != 0,
                    })
                })
                .collect()
        };
        parse().unwrap_or_default()
    }

    /// The name of the function each import address table slot holds, by the slot's RVA
    pub fn imports(&self) -> HashMap<usize, String> {
        let data = &self.data;
        let mut imports = HashMap::new();
        let parse = |imports: &mut HashMap<usize, String>| -> io::Result<()> {
            let pe = u32_at(data, 0x3C)? as usize;
            let optional_header = pe + 24;
            let directories = optional_header + if self.is_64 { 112 } else { 96 };
            let mut descriptor = u32_at(data, directories + 8)? as usize;
            let slot_size = if self.is_64 { 8 } else { 4 };
            let slot_at = |offset: usize| -> io::Result<u64> {
                if self.is_64 {
                    u64_at(data, offset)
                } else {
                    u32_at(data, offset).map(u64::from)
                }
            };
            let ordinal_flag = if self.is_64 { 1 << 63 } else { 1 << 31 };
            while descriptor != 0 && u32_at(data, descriptor + 12)? != 0 {
                // Names come from the import lookup table, since a loaded image's address table holds the addresses
                let lookup = match u32_at(data, descriptor)? as usize {
                    0 => u32_at(data, descriptor + 16)? as usize,
                    lookup => lookup,
                };
                let table = u32_at(data, descriptor + 16)? as usize;
                for i in 0.. {
                    let entry = slot_at(lookup + i * slot_size)?;
                    if entry == 0 {
                        break;
                    }
                    if entry & ordinal_flag == 0 {
                        let name = (entry as usize + 2..data.len().min(entry as usize + 258))
                            .find(|&end| data[end] == 0)
                            .map(|end| String::from_utf8_lossy(&data[entry as usize + 2..end]).into_owned());
                        if let Some(name) = name {
                            imports.insert(table + i * slot_size, name);
                        }
                    }
                }
                descriptor += 20;
            }
            Ok(())
        };
        let _ = parse(&mut imports);
        imports
    }
}

const SECTION_EXECUTE: u32 = 0x2000_0000;
const SECTION_WRITE: u32 = 0x8000_0000;

/// Where a section is (as RVAs) and what it holds
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub start: usize,
    pub end: usize,
    pub executable: bool,
    pub writable: bool,
}
