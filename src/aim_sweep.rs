//! TR2013's aim sweep: while Lara aims, the game sweeps a thin sphere forward from her head each frame and holds her back
//! by however far it gets into a wall (like ROTTR's weapon proxy, which is a body instead). Its shape is tuning data in
//! the equipped weapon's type, which is found the way the game's getter (TR2013 Steam mod+0x56AE20) does when no weapon
//! is held through her weapon slot: her own handle -> her inventory record -> its map, keyed by its current item -> the
//! weapon's instance -> its type record -> the tuning block. Where everything is is in `layout::AimSweepLayout`.

use crate::collision::AttachedProxy;
use crate::game_memory::{Handle, pointer_size, read_floats, read_pointer, read_u32};
use crate::layout;
use crate::object_list;
use crate::ui_ipc_data::Capsule;
use nalgebra::Vector3;

// Map nodes followed at most
const MAX_NODES: usize = 64;
// How far from Lara a tuning block's points can be, and how long its sweep
const MAX_SWEEP: f32 = 500.0;
// How short its sweep and how big its radius can be
const MIN_SWEEP: f32 = 10.0;
const MAX_RADIUS: f32 = 100.0;
// How far she has to be pushed (horizontally, in a frame) to count as pushed
const MIN_PUSH: f32 = 0.05;

// Pools' indices, ids and map keys are u32s
fn read_index(handle: Handle, address: usize) -> Option<usize> {
    read_u32(handle, address).map(|v| v as usize)
}

// A handle: a u32 index, then a u32 id
fn read_handle(handle: Handle, address: usize) -> Option<(usize, usize)> {
    Some((read_index(handle, address)?, read_index(handle, address + 4)?))
}

/// The record a handle (index, id) refers to in a pool, if it's still that one: the pool's handle entries say which
/// record, and the record says which index and id it is
fn pool_record(handle: Handle, pool: usize, (index, id): (usize, usize), record_size: usize, record_index: usize) -> Option<usize> {
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
    (read_handle(handle, record + record_index)? == (inner, id)).then(|| record)
}

/// The instance a handle refers to, if it's still that one
fn instance(handle: Handle, instances: usize, (index, id): (usize, usize)) -> Option<usize> {
    let layout = &layout::current().aim_sweep;
    if id == 0 || index >= layout.max_handles {
        return None;
    }
    let instance = instances + layout.instance_base + index * layout::current().instance.size;
    (read_handle(handle, instance + layout.instance_index)? == (index, id)).then(|| instance)
}

/// The tuning block of the weapon Lara has out, if she has one out
fn tuning_block(handle: Handle, instances: usize, inventories: usize, types: usize, lara: usize) -> Option<usize> {
    let layout = &layout::current().aim_sweep;
    let lara_handle = read_handle(handle, lara + layout.type_handle)?;
    let inventory = pool_record(handle, inventories, lara_handle, layout.inventory_size, layout.inventory_index)?;
    let key = read_index(handle, inventory + layout.current_item)?;
    let buckets = read_pointer(handle, inventory + layout.map_buckets)?;
    let bucket_count = read_index(handle, inventory + layout.map_bucket_count)?;
    if key == 0 || bucket_count == 0 {
        return None;
    }
    // A null node is the end of the bucket
    let mut node = read_pointer(handle, buckets + (key % bucket_count) * pointer_size())?;
    for _ in 0..MAX_NODES {
        if read_index(handle, node + layout.node_key)? == key {
            break;
        }
        node = read_pointer(handle, node + layout.node_next)?;
    }
    let weapon = instance(handle, instances, read_handle(handle, node + layout.node_item)?)?;
    // The weapon pool has a record per weapon instance, by its handle, with its tuning
    let weapon_handle = read_handle(handle, weapon + layout.type_handle)?;
    let record = pool_record(handle, types, weapon_handle, layout.type_size, layout.type_index)?;
    Some(read_pointer(handle, record + layout.type_data)? + layout.block)
}

// A tuning block's sweep in Lara's space (start, end, radius), if it's a sane one
fn read_sweep(handle: Handle, block: usize) -> Option<([f32; 3], [f32; 3], f32)> {
    let layout = &layout::current().aim_sweep;
    let start = read_floats::<3>(handle, block + layout.start)?;
    let middle = read_floats::<3>(handle, block + layout.middle)?;
    let [radius] = read_floats::<1>(handle, block + layout.radius)?;
    // It's swept from the start to the middle point less the radius
    let end = [middle[0], middle[1] - radius, middle[2]];
    let near = |p: &[f32; 3]| p.iter().all(|v| v.is_finite() && v.abs() < MAX_SWEEP);
    let length = (Vector3::from(end) - Vector3::from(start)).norm();
    let sane = near(&start)
        && near(&middle)
        && radius > 0.0
        && radius < MAX_RADIUS
        && length > MIN_SWEEP
        && length < MAX_SWEEP;
    sane.then(|| (start, end, radius))
}

/// The aim sweep in world space, as a capsule, if Lara has a weapon out, and whether it's pushing her. `lara` is her
/// instance.
pub fn read(handle: Handle, instances: usize, inventories: usize, types: usize, lara: usize) -> Option<AttachedProxy> {
    let layout = &layout::current().aim_sweep;
    let block = tuning_block(handle, instances, inventories, types, lara)?;
    let (start, end, radius) = read_sweep(handle, block)?;
    let m = object_list::read_transform(handle, lara).ok()?;
    let to_world = |p: [f32; 3]| object_list::to_world(&m, Vector3::from(p));
    // The push is added to the player's movement this frame, as one kind of it
    let pushing = read_pointer(handle, lara + layout.instance_player)
        .and_then(|p| read_pointer(handle, p + layout.player))
        .and_then(|player| read_floats::<2>(handle, player + layout.push))
        .map_or(false, |[x, y]| (x * x + y * y).sqrt() > MIN_PUSH);
    Some(AttachedProxy { capsule: Capsule { a: to_world(start), b: to_world(end), radius }, touching: pushing })
}
