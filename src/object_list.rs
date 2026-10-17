use crate::position::Position;
use nalgebra::{Rotation3, Vector3};
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::HashMap;
use std::convert::TryInto;
use std::io;

// Layout of the game's instance (game object) records, found in ROTTR (Steam) by following the RTTI
// named components they point to. The records live in a static array; Lara is one of them.
const INSTANCE_SIZE: usize = 0x120;
const INSTANCE_HEADER: usize = 0x0; // 2 for every slot in the array, 0 after the end
const INSTANCE_POSITION: usize = 0x10;
const INSTANCE_ROTATION: usize = 0x30; // Euler angles in radians, applied as Rx * Ry * Rz
const INSTANCE_PLACEMENT: usize = 0x78; // -> placement record, null for spawned instances
const INSTANCE_TRANSFORM: usize = 0xE0; // -> cdc::TransformComponent, null for unused slots
const PLACEMENT_NAME: usize = 0x48; // -> e.g. "fx_fire_campfire_prelight1-neberle2"
const PLACEMENT_FULL_NAME: usize = 0x50; // -> e.g. "campsite_base_trx.fx_fire_campfire-neberle7"
const TRANSFORM_MATRIX: usize = 0x30; // Row-major 4x4 object-to-world matrix, not always up to date

// Object space bounds come from the render model's mesh: instance -> component table -> DrawableComponent
// -> InstanceDrawable -> CommonRenderModelInstance -> CommonRenderModel -> Mesh
const INSTANCE_COMPONENTS: usize = 0xD8;
const COMPONENTS_DRAWABLE: usize = 0x40; // The table has a fixed slot per component type
const DRAWABLE_INSTANCE_DRAWABLE: usize = 0x98; // Null for instances that aren't drawn (triggers, markers, ...)
const INSTANCE_DRAWABLE_MODEL_INSTANCE: usize = 0xC8;
const MODEL_INSTANCE_MODEL: usize = 0x48;
const MODEL_MESH: usize = 0x50;
const MESH_MAGIC: &[u8; 4] = b"Mesh";
const MESH_MIN: usize = 0x20;
const MESH_MAX: usize = 0x30;

const MAX_INSTANCES: usize = 16384;
const SLOTS_PER_READ: usize = 512;

#[derive(Debug, Clone)]
pub struct Instance {
    pub address: usize,
    pub position: Position,
    pub name: String,
    pub full_name: String,
    // Object space (min, max), see `read_bounds`
    pub bounds: Option<(Position, Position)>,
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
                    position: read_position_from(slot, INSTANCE_POSITION),
                    name: self.name(handle, read_usize(&name_pointers, 0)),
                    full_name: self.name(handle, read_usize(&name_pointers, PLACEMENT_FULL_NAME - PLACEMENT_NAME)),
                    bounds: None,
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

// The game keeps the instance's own position and rotation up to date, but not always its TransformComponent
// (Lara's stays where she loaded in), so that's only used for the scale.
pub fn read_transform(handle: ProcessHandle, instance_address: usize) -> io::Result<[f32; 16]> {
    let mut instance = [0u8; INSTANCE_TRANSFORM + 8];
    handle.copy_address(instance_address, &mut instance)?;
    let transform = read_usize(&instance, INSTANCE_TRANSFORM);
    if transform == 0 {
        return Err(io::Error::new(io::ErrorKind::NotFound, "instance has no transform"));
    }
    let mut axes = [0u8; 48];
    handle.copy_address(transform + TRANSFORM_MATRIX, &mut axes)?;

    let position = read_position_from(&instance, INSTANCE_POSITION);
    let rotation = read_position_from(&instance, INSTANCE_ROTATION);
    let rotation = Rotation3::from_axis_angle(&Vector3::x_axis(), rotation.x)
        * Rotation3::from_axis_angle(&Vector3::y_axis(), rotation.y)
        * Rotation3::from_axis_angle(&Vector3::z_axis(), rotation.z);

    // Row-major for row vectors like the game's, so each row is an object axis in world space
    let mut matrix = [0f32; 16];
    for axis in 0..3 {
        let old_axis = read_position_from(&axes, axis * 16);
        let scale = Vector3::new(old_axis.x, old_axis.y, old_axis.z).norm();
        let direction = rotation.matrix().column(axis) * scale;
        matrix[axis * 4..axis * 4 + 3].copy_from_slice(direction.as_slice());
    }
    matrix[12..16].copy_from_slice(&[position.x, position.y, position.z, 1.0]);
    Ok(matrix)
}

pub fn read_position(handle: ProcessHandle, instance_address: usize) -> io::Result<Position> {
    let mut bytes = [0u8; 12];
    handle.copy_address(instance_address + INSTANCE_POSITION, &mut bytes)?;
    Ok(read_position_from(&bytes, 0))
}

// The bounding box of the instance's render model, None if it isn't drawn
pub fn read_bounds(handle: ProcessHandle, instance_address: usize) -> Option<(Position, Position)> {
    let components = read_pointer(handle, instance_address + INSTANCE_COMPONENTS)?;
    let drawable = read_pointer(handle, components + COMPONENTS_DRAWABLE)?;
    let instance_drawable = read_pointer(handle, drawable + DRAWABLE_INSTANCE_DRAWABLE)?;
    let model_instance = read_pointer(handle, instance_drawable + INSTANCE_DRAWABLE_MODEL_INSTANCE)?;
    let model = read_pointer(handle, model_instance + MODEL_INSTANCE_MODEL)?;
    let mesh = read_pointer(handle, model + MODEL_MESH)?;

    let mut bytes = [0u8; MESH_MAX + 12];
    handle.copy_address(mesh, &mut bytes).ok()?;
    if &bytes[..4] != MESH_MAGIC {
        return None;
    }
    Some((read_position_from(&bytes, MESH_MIN), read_position_from(&bytes, MESH_MAX)))
}

fn read_pointer(handle: ProcessHandle, address: usize) -> Option<usize> {
    let mut bytes = [0u8; 8];
    handle.copy_address(address, &mut bytes).ok()?;
    match read_usize(&bytes, 0) {
        0 => None,
        pointer => Some(pointer),
    }
}

fn read_position_from(bytes: &[u8], offset: usize) -> Position {
    Position {
        x: read_f32(bytes, offset),
        y: read_f32(bytes, offset + 4),
        z: read_f32(bytes, offset + 8),
    }
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
