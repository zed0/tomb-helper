use crate::position::Position;
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::HashMap;
use std::convert::TryInto;
use std::io;

// Layout of the game's instance (game object) records, found in ROTTR (Steam) by following the RTTI
// named components they point to. The records live in a static array; Lara is one of them.
const INSTANCE_SIZE: usize = 0x120;
const INSTANCE_HEADER: usize = 0x0; // 2 for every slot in the array, 0 after the end
const INSTANCE_POSITION: usize = 0x10;
const INSTANCE_PLACEMENT: usize = 0x78; // -> placement record, null for spawned instances
const INSTANCE_TRANSFORM: usize = 0xE0; // -> cdc::TransformComponent, null for unused slots
const PLACEMENT_NAME: usize = 0x48; // -> e.g. "fx_fire_campfire_prelight1-neberle2"
const PLACEMENT_FULL_NAME: usize = 0x50; // -> e.g. "campsite_base_trx.fx_fire_campfire-neberle7"
const TRANSFORM_MATRIX: usize = 0x30; // Row-major 4x4 object-to-world matrix

const MAX_INSTANCES: usize = 16384;
const SLOTS_PER_READ: usize = 512;

#[derive(Debug, Clone)]
pub struct Instance {
    pub address: usize,
    pub position: Position,
    pub name: String,
    pub full_name: String,
}

pub struct ObjectList {
    array_address: usize,
    // Placement names never change, so cache them by string address
    names: HashMap<usize, String>,
}

impl ObjectList {
    pub fn new(array_address: usize) -> ObjectList {
        ObjectList {
            array_address,
            names: HashMap::new(),
        }
    }

    // Every live instance with a placement (i.e. a name)
    pub fn instances(&mut self, handle: ProcessHandle) -> io::Result<Vec<Instance>> {
        let mut instances = vec![];
        let mut bytes = vec![0u8; INSTANCE_SIZE * SLOTS_PER_READ];
        for first_slot in (0..MAX_INSTANCES).step_by(SLOTS_PER_READ) {
            handle.copy_address(self.array_address + first_slot * INSTANCE_SIZE, &mut bytes)?;
            for (index, slot) in bytes.chunks_exact(INSTANCE_SIZE).enumerate() {
                if read_u32(slot, INSTANCE_HEADER) == 0 {
                    return Ok(instances);
                }
                let placement = read_usize(slot, INSTANCE_PLACEMENT);
                if read_usize(slot, INSTANCE_TRANSFORM) == 0 || placement == 0 {
                    continue;
                }

                let mut name_pointers = [0u8; 16];
                if handle.copy_address(placement + PLACEMENT_NAME, &mut name_pointers).is_err() {
                    continue;
                }
                instances.push(Instance {
                    address: self.array_address + (first_slot + index) * INSTANCE_SIZE,
                    position: Position {
                        x: read_f32(slot, INSTANCE_POSITION),
                        y: read_f32(slot, INSTANCE_POSITION + 4),
                        z: read_f32(slot, INSTANCE_POSITION + 8),
                    },
                    name: self.name(handle, read_usize(&name_pointers, 0)),
                    full_name: self.name(handle, read_usize(&name_pointers, PLACEMENT_FULL_NAME - PLACEMENT_NAME)),
                });
            }
        }
        Ok(instances)
    }

    fn name(&mut self, handle: ProcessHandle, address: usize) -> String {
        if address == 0 {
            return String::new();
        }
        self.names
            .entry(address)
            .or_insert_with(|| read_c_string(handle, address))
            .clone()
    }
}

pub fn read_transform(handle: ProcessHandle, instance_address: usize) -> io::Result<[f32; 16]> {
    let mut pointer = [0u8; 8];
    handle.copy_address(instance_address + INSTANCE_TRANSFORM, &mut pointer)?;
    let transform = read_usize(&pointer, 0);
    if transform == 0 {
        return Err(io::Error::new(io::ErrorKind::NotFound, "instance has no transform"));
    }
    let mut bytes = [0u8; 64];
    handle.copy_address(transform + TRANSFORM_MATRIX, &mut bytes)?;
    let mut matrix = [0f32; 16];
    for (i, element) in matrix.iter_mut().enumerate() {
        *element = read_f32(&bytes, i * 4);
    }
    Ok(matrix)
}

fn read_c_string(handle: ProcessHandle, address: usize) -> String {
    // A long read can fail if the string is near the end of a page, so fall back to a short one
    for length in [128, 16] {
        let mut bytes = vec![0u8; length];
        if handle.copy_address(address, &mut bytes).is_ok() {
            let end = bytes.iter().position(|&c| c == 0).unwrap_or(length);
            return String::from_utf8_lossy(&bytes[..end]).into_owned();
        }
    }
    String::new()
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_usize(bytes: &[u8], offset: usize) -> usize {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize
}
