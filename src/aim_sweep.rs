//! TR2013's aim sweep: while Lara aims, the game sweeps a thin sphere forward from her head each frame and holds her back
//! by however far it gets into a wall (like ROTTR's weapon proxy, which is a body instead). Its shape is tuning data in
//! the equipped weapon's type, which is found the way the game's getter (TR2013 Steam mod+0x56AE20) does when no weapon
//! is held through her weapon slot: her own handle -> her inventory record -> its map, keyed by its current item -> the
//! weapon's instance -> its type record -> the tuning block. Where everything is is in `layout::AimSweepLayout`.

use crate::game_memory::{Handle, read_floats, read_u32};
use crate::layout;
use crate::object_list;
use crate::position::Position;
use crate::ui_ipc_data::Capsule;
use nalgebra::Vector3;

// Map nodes followed at most
const MAX_NODES: usize = 64;
// How far she has to be pushed (horizontally, in a frame) to count as pushed
const MIN_PUSH: f32 = 0.05;


/// The record a handle (index, id) refers to in a pool, if it's still that one: the pool's handle entries say which
/// record, and the record says which index and id it is
// Pools' indices and ids are u32s
fn read_index(handle: Handle, address: usize) -> Option<usize> {
    read_u32(handle, address).map(|v| v as usize)
}

fn pool_record(handle: Handle, pool: usize, index: usize, id: usize, record_size: usize, record_index: usize) -> Option<usize> {
    let layout = &layout::current().aim_sweep;
    if id == 0 || index >= layout.max_handles {
        return None;
    }
    let entry = pool + index * layout.handle_size;
    if read_index(handle, entry + layout.handle_id)? != id {
        return None;
    }
    let inner = read_index(handle, entry)?;
    let record = pool + layout.records + inner * record_size;
    let checks = (read_index(handle, record + record_index)?, read_index(handle, record + record_index + 4)?);
    (checks == (inner, id)).then(|| record)
}

/// The instance a handle refers to, if it's still that one
fn instance(handle: Handle, instances: usize, index: usize, id: usize) -> Option<usize> {
    let layout = &layout::current().aim_sweep;
    if id == 0 || index >= layout.max_handles {
        return None;
    }
    let instance = instances + layout.instance_base + index * layout::current().instance.size;
    let checks = (read_index(handle, instance + layout.instance_index)?, read_index(handle, instance + layout.instance_index + 4)?);
    (checks == (index, id)).then(|| instance)
}

/// The tuning block of the weapon Lara has out, if she has one out
fn tuning_block(handle: Handle, instances: usize, inventories: usize, types: usize, lara: usize) -> Option<usize> {
    let layout = &layout::current().aim_sweep;
    let (index, id) = (read_index(handle, lara + layout.type_handle)?, read_index(handle, lara + layout.type_handle + 4)?);
    let inventory = pool_record(handle, inventories, index, id, layout.inventory_size, layout.inventory_index)?;
    let key = read_index(handle, inventory + layout.current_item)?;
    let buckets = read_index(handle, inventory + layout.map_buckets)?;
    let bucket_count = read_index(handle, inventory + layout.map_bucket_count)?;
    if key == 0 || bucket_count == 0 {
        return None;
    }
    let mut node = read_index(handle, buckets + (key % bucket_count) * 4)?;
    for _ in 0..MAX_NODES {
        if node == 0 {
            return None;
        }
        if read_index(handle, node + layout.node_key)? == key {
            break;
        }
        node = read_index(handle, node + layout.node_next)?;
    }
    let weapon_handle = (read_index(handle, node + layout.node_item)?, read_index(handle, node + layout.node_item + 4)?);
    let weapon = instance(handle, instances, weapon_handle.0, weapon_handle.1)?;
    let type_handle = (read_index(handle, weapon + layout.type_handle)?, read_index(handle, weapon + layout.type_handle + 4)?);
    let record = pool_record(handle, types, type_handle.0, type_handle.1, layout.type_size, layout.type_index)?;
    Some(read_index(handle, record + layout.type_data)? + layout.block)
}

/// The aim sweep in world space, as a capsule, if Lara has a weapon out, and whether it's pushing her. `lara` is her
/// instance.
pub fn read(handle: Handle, instances: usize, inventories: usize, types: usize, lara: usize) -> Option<(Capsule, bool)> {
    let layout = &layout::current().aim_sweep;
    let block = tuning_block(handle, instances, inventories, types, lara)?;
    let start = read_floats::<3>(handle, block + layout.start)?;
    let middle = read_floats::<3>(handle, block + layout.middle)?;
    let radius = read_floats::<1>(handle, block + layout.radius)?[0];
    if !(radius > 0.0 && radius < 100.0) {
        return None;
    }
    // It's swept from the start to the middle point less the radius, in her space
    let end = [middle[0], middle[1] - radius, middle[2]];
    let m = object_list::read_transform(handle, lara).ok()?;
    let to_world = |p: [f32; 3]| {
        let v = Vector3::new(m[12], m[13], m[14])
            + Vector3::new(m[0], m[1], m[2]) * p[0]
            + Vector3::new(m[4], m[5], m[6]) * p[1]
            + Vector3::new(m[8], m[9], m[10]) * p[2];
        Position { x: v.x, y: v.y, z: v.z }
    };
    // The push is added to the player's movement this frame, as one kind of it
    let pushing = read_index(handle, lara + layout.instance_player)
        .and_then(|p| read_index(handle, p + layout.player))
        .and_then(|player| read_floats::<2>(handle, player + layout.push))
        .map_or(false, |[x, y]| (x * x + y * y).sqrt() > MIN_PUSH);
    Some((Capsule { a: to_world(start), b: to_world(end), radius }, pushing))
}
