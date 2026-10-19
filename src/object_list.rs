use crate::components::{self, Components};
use crate::layout;
use crate::position::Position;
use nalgebra::{Rotation3, Vector3};
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::HashMap;
use std::convert::TryInto;
use std::io;

// The game's instances (game objects) live in a static array (`InstanceArray`); Lara is one of them. Where things are
// in them is in `layout`. Object space bounds come from the render model's mesh, via the DrawableComponent.
const MESH_MAGIC: &[u8; 4] = b"Mesh";

const MAX_INSTANCES: usize = 16384;
const SLOTS_PER_READ: usize = 512;

#[derive(Debug, Clone)]
pub struct Instance {
    pub address: usize,
    // Unique, and what scripts refer to it by
    pub id: u64,
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

    // Every live instance. Placed ones are named after their placement, spawned ones after their object type.
    pub fn instances(&mut self, handle: ProcessHandle) -> io::Result<Vec<Instance>> {
        let layout = layout::current();
        let (instance, placement_layout) = (&layout.instance, &layout.placement);
        let mut instances = vec![];
        let mut bytes = vec![0u8; instance.size * SLOTS_PER_READ];
        for first_slot in (0..MAX_INSTANCES).step_by(SLOTS_PER_READ) {
            handle.copy_address(self.array_address + first_slot * instance.size, &mut bytes)?;
            for (index, slot) in bytes.chunks_exact(instance.size).enumerate() {
                if read_u32(slot, instance.header) == 0 {
                    return Ok(instances);
                }
                if read_usize(slot, instance.in_use) == 0 {
                    continue;
                }
                let address = self.array_address + (first_slot + index) * instance.size;
                let placement = instance.placement.map_or(0, |offset| read_usize(slot, offset));
                let (name, full_name) = match placement {
                    0 => (String::new(), String::new()),
                    _ => {
                        let name = read_pointer(handle, placement + placement_layout.name).unwrap_or(0);
                        let full_name = read_pointer(handle, placement + placement_layout.full_name).unwrap_or(0);
                        (self.name(handle, name), self.name(handle, full_name))
                    }
                };
                // Spawned instances have no placement, and SOTTR's placements are all called "?", so they go by what
                // they are instead
                let unnamed = |n: &str| n.is_empty() || n == "?";
                let (name, full_name) = if unnamed(&name) {
                    match read_chain(handle, address, &instance.object_name) {
                        Some(object_name) => (self.name(handle, object_name), String::new()),
                        None => continue,
                    }
                } else {
                    (name, if unnamed(&full_name) { String::new() } else { full_name })
                };
                instances.push(Instance {
                    address,
                    id: u64::from_le_bytes(slot[instance.id..instance.id + 8].try_into().unwrap()),
                    position: read_position_from(slot, instance.position),
                    name,
                    full_name,
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
    let layout = layout::current();
    let mut instance = vec![0u8; layout.instance.size];
    handle.copy_address(instance_address, &mut instance)?;
    // Unscaled if instances don't point at their transform
    let mut axes = [0u8; 48];
    match layout.instance.transform {
        Some(offset) => {
            let transform = read_usize(&instance, offset);
            if transform == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "instance has no transform"));
            }
            handle.copy_address(transform + layout.transform.matrix, &mut axes)?;
        }
        None => {
            for axis in 0..3 {
                axes[axis * 16 + axis * 4..axis * 16 + axis * 4 + 4].copy_from_slice(&1f32.to_le_bytes());
            }
        }
    }

    let position = read_position_from(&instance, layout.instance.position);
    let rotation = read_position_from(&instance, layout.instance.rotation);
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
    handle.copy_address(instance_address + layout::current().instance.position, &mut bytes)?;
    Ok(read_position_from(&bytes, 0))
}

// The bounding box of the instance's render model, None if it isn't drawn
pub fn read_bounds(
    handle: ProcessHandle,
    components: &mut Components,
    instance_address: usize,
) -> Option<(Position, Position)> {
    let layout = &layout::current().drawable;
    let drawable = components.get(handle, instance_address, components::DRAWABLE)?;
    let instance_drawable = read_pointer(handle, drawable + layout.instance_drawable)?;
    let model_instance = read_pointer(handle, instance_drawable + layout.model_instance)?;
    let model = read_pointer(handle, model_instance + layout.model)?;
    let mesh = read_pointer(handle, model + layout.mesh)?;

    let mut bytes = vec![0u8; layout.mesh_min.max(layout.mesh_max) + 12];
    handle.copy_address(mesh, &mut bytes).ok()?;
    if &bytes[..4] != MESH_MAGIC {
        return None;
    }
    Some((read_position_from(&bytes, layout.mesh_min), read_position_from(&bytes, layout.mesh_max)))
}

// Follows the pointers at each offset in turn
fn read_chain(handle: ProcessHandle, address: usize, offsets: &[usize]) -> Option<usize> {
    offsets.iter().try_fold(address, |address, offset| read_pointer(handle, address + offset))
}

fn read_pointer(handle: ProcessHandle, address: usize) -> Option<usize> {
    let mut bytes = vec![0u8; layout::pointer_size()];
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

// A pointer, at the game's pointer size
fn read_usize(bytes: &[u8], offset: usize) -> usize {
    layout::pointer_from(&bytes[offset..])
}
