//! The game's instances (game objects) from its static array (`InstanceArray`): position, names (placement, or object
//! type for spawned ones), world matrix (built from the instance's position and Euler rotation, Rx * Ry * Rz, with only
//! the scale from its `cdc::TransformComponent`, which goes stale for Lara), and model bounds (`read_bounds`: component
//! table -> DrawableComponent -> InstanceDrawable -> CommonRenderModelInstance -> CommonRenderModel -> the `Mesh`
//! header). Where everything is is in the layout (`layout::InstanceLayout`).

use crate::game_memory::{Handle, pointer_at, read_c_string, read_pointer, u32_at, u64_at, vector_at};
use crate::components::{self, Components};
use crate::layout;
use crate::position::Position;
use nalgebra::{Rotation3, Vector3};
use process_memory::CopyAddress;
use std::collections::HashMap;
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
    pub fn instances(&mut self, handle: Handle) -> io::Result<Vec<Instance>> {
        let layout = layout::current();
        let (instance, placement_layout) = (&layout.instance, &layout.placement);
        let words = crate::words::current();
        let mut instances = vec![];
        let mut bytes = vec![0u8; instance.size * SLOTS_PER_READ];
        for first_slot in (0..MAX_INSTANCES).step_by(SLOTS_PER_READ) {
            handle.copy_address(self.array_address + first_slot * instance.size, &mut bytes)?;
            for (index, slot) in bytes.chunks_exact(instance.size).enumerate() {
                if u32_at(slot, instance.header) == 0 {
                    return Ok(instances);
                }
                if pointer_at(slot, instance.in_use) == 0 {
                    continue;
                }
                let address = self.array_address + (first_slot + index) * instance.size;
                let placement = instance.placement.map_or(0, |offset| pointer_at(slot, offset));
                let (name, full_name) = match placement {
                    0 => (String::new(), String::new()),
                    _ => {
                        let name = read_pointer(handle, placement + placement_layout.name).unwrap_or(0);
                        let full_name = read_pointer(handle, placement + placement_layout.full_name).unwrap_or(0);
                        (self.name(handle, name), self.name(handle, full_name))
                    }
                };
                // Spawned instances have no placement, and some games' placements all have a placeholder name
                // (`words.placeholder_names`), so they go by what they are instead
                let unnamed = |n: &str| n.is_empty() || words.placeholder_names.iter().any(|p| p == n);
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
                    id: u64_at(&slot, instance.id),
                    position: read_position_from(slot, instance.position),
                    name,
                    full_name,
                    bounds: None,
                });
            }
        }
        Ok(instances)
    }

    fn name(&mut self, handle: Handle, address: usize) -> String {
        if address == 0 {
            return String::new();
        }
        self.names
            .entry(address)
            .or_insert_with(|| read_c_string(handle, address))
            .clone()
    }
}

/// Sorts the instances nearest `to` first
pub fn sort_by_distance(instances: &mut [Instance], to: &Position) {
    instances.sort_by(|a, b| a.position.distance(to).total_cmp(&b.position.distance(to)));
}

// The game keeps the instance's own position and rotation up to date, but not always its TransformComponent
// (Lara's stays where she loaded in), so that's only used for the scale.
pub fn read_transform(handle: Handle, instance_address: usize) -> io::Result<[f32; 16]> {
    let layout = layout::current();
    let mut instance = vec![0u8; layout.instance.size];
    handle.copy_address(instance_address, &mut instance)?;
    // Each axis' scale, unscaled if instances don't point at their transform
    let scale = match layout.instance.transform {
        Some(offset) => {
            let transform = pointer_at(&instance, offset);
            if transform == 0 {
                return Err(crate::game_memory::unreadable("instance has no transform"));
            }
            // Its first three rows are the object's axes
            let mut axes = [0u8; 3 * 16];
            handle.copy_address(transform + layout.transform.matrix, &mut axes)?;
            [0, 1, 2].map(|axis| vector_at(&axes, axis * 16).norm())
        }
        None => [1.0; 3],
    };

    let position = read_position_from(&instance, layout.instance.position);
    let rotation = read_position_from(&instance, layout.instance.rotation);
    let rotation = Rotation3::from_axis_angle(&Vector3::x_axis(), rotation.x)
        * Rotation3::from_axis_angle(&Vector3::y_axis(), rotation.y)
        * Rotation3::from_axis_angle(&Vector3::z_axis(), rotation.z);

    // Row-major for row vectors like the game's, so each row is an object axis in world space
    let mut matrix = [0f32; 16];
    for axis in 0..3 {
        let direction = rotation.matrix().column(axis) * scale[axis];
        matrix[axis * 4..axis * 4 + 3].copy_from_slice(direction.as_slice());
    }
    matrix[12..16].copy_from_slice(&[position.x, position.y, position.z, 1.0]);
    Ok(matrix)
}

/// A point in an object's space in world space, for its transform (`read_transform`'s row-major matrix of row vectors)
pub fn to_world(transform: &[f32; 16], point: Vector3<f32>) -> Position {
    let row = |r: usize| Vector3::new(transform[r * 4], transform[r * 4 + 1], transform[r * 4 + 2]);
    Position::from(row(0) * point.x + row(1) * point.y + row(2) * point.z + row(3))
}

/// Where an object's origin is in world space, for its transform (its last row)
pub fn origin(transform: &[f32; 16]) -> Position {
    Position { x: transform[12], y: transform[13], z: transform[14] }
}

pub fn read_position(handle: Handle, instance_address: usize) -> io::Result<Position> {
    let mut bytes = [0u8; 12];
    handle.copy_address(instance_address + layout::current().instance.position, &mut bytes)?;
    Ok(read_position_from(&bytes, 0))
}

// The bounding box of the instance's render model, None if it isn't drawn
pub fn read_bounds(
    handle: Handle,
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

/// What an instance is (its object type's name, e.g. "laracroft")
pub fn object_name(handle: Handle, instance_address: usize) -> Option<String> {
    let address = read_chain(handle, instance_address, &layout::current().instance.object_name)?;
    Some(read_c_string(handle, address))
}

// Follows the pointers at each offset in turn
fn read_chain(handle: Handle, address: usize, offsets: &[usize]) -> Option<usize> {
    offsets.iter().try_fold(address, |address, offset| read_pointer(handle, address + offset))
}

fn read_position_from(bytes: &[u8], offset: usize) -> Position {
    Position::from(vector_at(bytes, offset))
}

