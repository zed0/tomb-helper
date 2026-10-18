use crate::object_list::{INSTANCE_COMPONENTS, INSTANCE_TRANSFORM};
use crate::position::Position;
use crate::ui_ipc_data::{Capsule, Probe};
use nalgebra::{Quaternion, UnitQuaternion, Vector3};
use process_memory::{CopyAddress, ProcessHandle};
use std::convert::TryInto;
use std::io;

// Lara's hit capsules ("DefensiveCapsule_head", ...) are listed in her CollideComponent, each attached to a
// bone of her skeleton. Found in ROTTR (Steam).
const COMPONENTS_COLLIDE: usize = 0x50; // Fixed slot in the instance's component table
const COLLIDE_SHAPES: usize = 0x70; // -> array of SHAPE_ENTRY_SIZE entries
const COLLIDE_SHAPE_COUNT: usize = 0x78; // u32
const SHAPE_ENTRY_SIZE: usize = 0x18;
const SHAPE_ENTRY_ID: usize = 0x8; // Byte 2 is the shape's type, the high u32 its bone
const SHAPE_ENTRY_SHAPE: usize = 0x10; // -> shape in bone space
const SHAPE_TYPE_CAPSULE: u8 = 0x8A; // The others are spheres used for attacks (melee, parry, kicks)
const CAPSULE_CENTRE: usize = 0x0;
const CAPSULE_ROTATION: usize = 0x10; // Quaternion x, y, z, w; the capsule runs along its z axis
const CAPSULE_RADIUS: usize = 0x28;
const CAPSULE_LENGTH: usize = 0x2C; // Of the cylinder part
const CAPSULE_SIZE: usize = 0x30;

// The TransformComponent's (unlike its own matrix) bone matrices are kept up to date. Row-major world space
// matrices, with an extra root first so shape bone n is matrix n + 1.
const TRANSFORM_BONES: usize = 0x80;
const MATRIX_SIZE: usize = 64;

#[derive(Debug, Clone)]
pub struct CapsuleShape {
    bone: usize,
    // Ends of the cylinder part, in bone space
    a: Vector3<f32>,
    b: Vector3<f32>,
    radius: f32,
}

// Shapes only change with Lara's model, so these can be read occasionally
pub fn read_capsule_shapes(handle: ProcessHandle, instance_address: usize) -> io::Result<Vec<CapsuleShape>> {
    let components = read_usize(handle, instance_address + INSTANCE_COMPONENTS)?;
    let collide = read_usize(handle, components + COMPONENTS_COLLIDE)?;
    if collide == 0 {
        return Ok(vec![]);
    }
    let mut header = [0u8; COLLIDE_SHAPE_COUNT + 4];
    handle.copy_address(collide, &mut header)?;
    let shapes = usize_at(&header, COLLIDE_SHAPES);
    let count = u32::from_le_bytes(header[COLLIDE_SHAPE_COUNT..COLLIDE_SHAPE_COUNT + 4].try_into().unwrap()) as usize;
    if shapes == 0 || count > 256 {
        return Ok(vec![]);
    }

    let mut entries = vec![0u8; count * SHAPE_ENTRY_SIZE];
    handle.copy_address(shapes, &mut entries)?;
    let mut capsules = vec![];
    for entry in entries.chunks_exact(SHAPE_ENTRY_SIZE) {
        let id = usize_at(entry, SHAPE_ENTRY_ID);
        if (id >> 16) as u8 != SHAPE_TYPE_CAPSULE {
            continue;
        }
        let mut shape = [0u8; CAPSULE_SIZE];
        handle.copy_address(usize_at(entry, SHAPE_ENTRY_SHAPE), &mut shape)?;

        let rotation = quaternion_at(&shape, CAPSULE_ROTATION);
        let centre = vector_at(&shape, CAPSULE_CENTRE);
        let half_axis = rotation * Vector3::z() * (f32_at(&shape, CAPSULE_LENGTH) / 2.0);
        capsules.push(CapsuleShape {
            bone: (id >> 32) + 1,
            a: centre - half_axis,
            b: centre + half_axis,
            radius: f32_at(&shape, CAPSULE_RADIUS),
        });
    }
    Ok(capsules)
}

// The shapes in world space for the current pose
pub fn place_capsules(
    handle: ProcessHandle,
    instance_address: usize,
    shapes: &[CapsuleShape],
) -> io::Result<Vec<Capsule>> {
    let bone_count = match shapes.iter().map(|s| s.bone + 1).max() {
        Some(count) => count,
        None => return Ok(vec![]),
    };
    let transform = read_usize(handle, instance_address + INSTANCE_TRANSFORM)?;
    let bones_address = read_usize(handle, transform + TRANSFORM_BONES)?;
    if bones_address == 0 {
        return Ok(vec![]);
    }
    let mut bones = vec![0u8; bone_count * MATRIX_SIZE];
    handle.copy_address(bones_address, &mut bones)?;

    Ok(shapes
        .iter()
        .map(|shape| {
            let bone = &bones[shape.bone * MATRIX_SIZE..(shape.bone + 1) * MATRIX_SIZE];
            Capsule {
                a: to_world(bone, &shape.a),
                b: to_world(bone, &shape.b),
                radius: shape.radius,
            }
        })
        .collect())
}

// Lara's movement collision is the capsule of her character proxy (cdc::ProxyComponent), which the game
// moves her with. Its slot in the component table is the proxy component type's id, a static u32
// (`ProxyComponentType`). It holds the shape in use, and the one it's changing to (e.g. when she starts
// aiming), which the shape in use eases towards over a few frames.
const PROXY_CENTRE: usize = 0x30; // Of the shape in use, in world space
const PROXY_SHAPES: [ProxyShape; 2] = [
    ProxyShape { size: 0x58, offset: 0x60, local_rotation: 0x80, rotation: 0x90 },
    ProxyShape { size: 0xC0, offset: 0xD0, local_rotation: 0xF0, rotation: 0x100 },
];
const PROXY_SIZE: usize = 0x150;
// The physics capsule the proxy is swept as, which the shape in use is copied to
const PROXY_BODY: usize = 0x148; // -> body, whose +0x20 -> shape
const BODY_SHAPE: usize = 0x20;
// Bodies are in a list, and other proxies are attached to Lara's: the camera (radius 30, moving from 15 in
// front of her to 30 behind when she aims) and her weapon (a thin rod). Both only collide while she aims.
const BODY_NEXT: usize = 0x30;
const BODY_PARENT: usize = 0x48;
// How far along the list to look for bodies attached to Lara's
const MAX_BODIES_SEARCHED: usize = 16;
const SHAPE_CENTRE: usize = 0x0; // World space
const SHAPE_ROTATION: usize = 0x20; // World space quaternion x, y, z, w; the capsule runs along its z axis
const SHAPE_RADIUS: usize = 0x44; // Then the length of the cylinder part
const SHAPE_TYPE: usize = 0x98;
const SHAPE_TYPE_CAPSULE_BODY: u8 = 1;
const SHAPE_SIZE: usize = 0xA0;
// Shapes this similar are drawn once
const PROXY_SAME_SHAPE: f32 = 0.01;

struct ProxyShape {
    size: usize, // Radius, then the length of the cylinder part (from the physics shape's volume)
    offset: usize, // Of the centre from Lara's feet, in her space
    local_rotation: usize, // Tilt of the capsule (e.g. lying down when rolling), quaternion x, y, z, w
    rotation: usize, // Lara's facing
}

pub struct ProxyCapsules {
    // The one in use, then the one it's changing to if that's different
    pub shapes: Vec<Capsule>,
    // Of the proxies attached to Lara's
    pub attached: Vec<Capsule>,
}

// The proxy's shapes in world space
pub fn read_proxy(handle: ProcessHandle, instance_address: usize, type_id_address: usize) -> io::Result<ProxyCapsules> {
    let mut type_id = [0u8; 4];
    handle.copy_address(type_id_address, &mut type_id)?;
    let components = read_usize(handle, instance_address + INSTANCE_COMPONENTS)?;
    let proxy = read_usize(handle, components + u32::from_le_bytes(type_id) as usize * 8)?;
    if proxy == 0 {
        return Ok(ProxyCapsules { shapes: vec![], attached: vec![] });
    }
    let mut bytes = [0u8; PROXY_SIZE];
    handle.copy_address(proxy, &mut bytes)?;

    // Lara's feet plus her rotated offset is the centre for most moves, but not e.g. rolls, so use the one
    // the game stores, and place the other shape relative to it
    let mut centre = vector_at(&bytes, PROXY_CENTRE);
    let in_use_offset = vector_at(&bytes, PROXY_SHAPES[0].offset);
    let mut capsules: Vec<(Vector3<f32>, Vector3<f32>, Capsule)> = vec![];

    // The physics shape is what's actually swept, so prefer it for the shape in use
    if let Some((capsule, physics_centre)) = read_physics_capsule(handle, usize_at(&bytes, PROXY_BODY)) {
        let size = Vector3::new(f32_at(&bytes, PROXY_SHAPES[0].size), f32_at(&bytes, PROXY_SHAPES[0].size + 4), 0.0);
        capsules.push((size, in_use_offset, capsule));
        centre = physics_centre;
    }

    for shape in &PROXY_SHAPES {
        let radius = f32_at(&bytes, shape.size);
        let length = f32_at(&bytes, shape.size + 4);
        let offset = vector_at(&bytes, shape.offset);
        let rotation = quaternion_at(&bytes, shape.rotation);
        let size = Vector3::new(radius, length, 0.0);
        if capsules.iter().any(|(s, o, _)| (s - size).norm() < PROXY_SAME_SHAPE && (o - offset).norm() < PROXY_SAME_SHAPE) {
            continue;
        }
        let shape_centre = centre + rotation * (offset - in_use_offset);
        let half_axis = rotation * quaternion_at(&bytes, shape.local_rotation) * Vector3::z() * (length / 2.0);
        capsules.push((
            size,
            offset,
            Capsule { a: to_position(&(shape_centre - half_axis)), b: to_position(&(shape_centre + half_axis)), radius },
        ));
    }
    Ok(ProxyCapsules {
        shapes: capsules.into_iter().map(|(_, _, capsule)| capsule).collect(),
        attached: read_attached_capsules(handle, usize_at(&bytes, PROXY_BODY)),
    })
}

// The capsules of bodies attached (directly or not) to `body`, which follow it in the list
fn read_attached_capsules(handle: ProcessHandle, body: usize) -> Vec<Capsule> {
    let mut family = vec![body];
    let mut capsules = vec![];
    let mut current = body;
    for _ in 0..MAX_BODIES_SEARCHED {
        current = match read_usize(handle, current + BODY_NEXT) {
            Ok(next) if next != 0 && next != body => next,
            _ => break,
        };
        match read_usize(handle, current + BODY_PARENT) {
            Ok(parent) if family.contains(&parent) => family.push(current),
            _ => continue,
        }
        if let Some((capsule, _)) = read_physics_capsule(handle, current) {
            capsules.push(capsule);
        }
    }
    capsules
}

// The capsule and its centre
fn read_physics_capsule(handle: ProcessHandle, body: usize) -> Option<(Capsule, Vector3<f32>)> {
    if body == 0 {
        return None;
    }
    let shape = read_usize(handle, body + BODY_SHAPE).ok()?;
    if shape == 0 {
        return None;
    }
    let mut bytes = [0u8; SHAPE_SIZE];
    handle.copy_address(shape, &mut bytes).ok()?;
    if bytes[SHAPE_TYPE] != SHAPE_TYPE_CAPSULE_BODY {
        return None;
    }
    let centre = vector_at(&bytes, SHAPE_CENTRE);
    let radius = f32_at(&bytes, SHAPE_RADIUS);
    let half_axis = quaternion_at(&bytes, SHAPE_ROTATION) * Vector3::z() * (f32_at(&bytes, SHAPE_RADIUS + 4) / 2.0);
    Some((
        Capsule { a: to_position(&(centre - half_axis)), b: to_position(&(centre + half_axis)), radius },
        centre,
    ))
}

fn quaternion_at(bytes: &[u8], offset: usize) -> UnitQuaternion<f32> {
    UnitQuaternion::from_quaternion(Quaternion::new(
        f32_at(bytes, offset + 12),
        f32_at(bytes, offset),
        f32_at(bytes, offset + 4),
        f32_at(bytes, offset + 8),
    ))
}

// The ground probes the game casts for Lara: one per foot (knee height to below the foot) and one for
// her body (above her head to below her feet). A static count followed by records at PROBES_RECORDS.
const PROBES_RECORDS: usize = 0x70;
const PROBE_SIZE: usize = 0x60;
const PROBE_HIT: usize = 0x10; // Can be stale, see `place_probes`
const PROBE_START: usize = 0x20;
const PROBE_END: usize = 0x30;
const MAX_PROBES: usize = 8;
// Hits further than this from their probe are left over from an earlier cast
const PROBE_HIT_TOLERANCE: f32 = 10.0;

pub fn read_probes(handle: ProcessHandle, probes_address: usize) -> io::Result<Vec<Probe>> {
    let mut bytes = vec![0u8; PROBES_RECORDS + MAX_PROBES * PROBE_SIZE];
    handle.copy_address(probes_address, &mut bytes)?;
    let count = (u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize).min(MAX_PROBES);
    Ok(bytes[PROBES_RECORDS..]
        .chunks_exact(PROBE_SIZE)
        .take(count)
        .map(|record| {
            let start = vector_at(record, PROBE_START);
            let end = vector_at(record, PROBE_END);
            let hit = vector_at(record, PROBE_HIT);
            let has_hit = distance_to_segment(&hit, &start, &end) <= PROBE_HIT_TOLERANCE;
            Probe {
                start: to_position(&start),
                end: to_position(&end),
                hit: to_position(&hit),
                has_hit,
            }
        })
        .collect())
}

fn distance_to_segment(point: &Vector3<f32>, start: &Vector3<f32>, end: &Vector3<f32>) -> f32 {
    let line = end - start;
    let t = ((point - start).dot(&line) / line.norm_squared().max(1e-6)).clamp(0.0, 1.0);
    (point - (start + line * t)).norm()
}

fn to_position(vector: &Vector3<f32>) -> Position {
    Position { x: vector.x, y: vector.y, z: vector.z }
}

// `point * matrix` for a row-major matrix of row vectors, like the game's
fn to_world(matrix: &[u8], point: &Vector3<f32>) -> Position {
    let world = vector_at(matrix, 0) * point.x
        + vector_at(matrix, 16) * point.y
        + vector_at(matrix, 32) * point.z
        + vector_at(matrix, 48);
    Position { x: world.x, y: world.y, z: world.z }
}

fn read_usize(handle: ProcessHandle, address: usize) -> io::Result<usize> {
    let mut bytes = [0u8; 8];
    handle.copy_address(address, &mut bytes)?;
    Ok(usize_at(&bytes, 0))
}

fn usize_at(bytes: &[u8], offset: usize) -> usize {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn vector_at(bytes: &[u8], offset: usize) -> Vector3<f32> {
    Vector3::new(f32_at(bytes, offset), f32_at(bytes, offset + 4), f32_at(bytes, offset + 8))
}
