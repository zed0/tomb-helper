use crate::object_list::{INSTANCE_COMPONENTS, INSTANCE_TRANSFORM};
use crate::position::Position;
use crate::ui_ipc_data::Capsule;
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

        let rotation = UnitQuaternion::from_quaternion(Quaternion::new(
            f32_at(&shape, CAPSULE_ROTATION + 12),
            f32_at(&shape, CAPSULE_ROTATION),
            f32_at(&shape, CAPSULE_ROTATION + 4),
            f32_at(&shape, CAPSULE_ROTATION + 8),
        ));
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
