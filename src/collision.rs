use crate::game_memory::{f32_at, pointer_at, read_pointer_or_null, u32_at, u64_at, vector_at};
use crate::components::{self, Components};
use crate::layout;
use crate::position::Position;
use crate::ui_ipc_data::{Capsule, Probe};
use nalgebra::{Quaternion, UnitQuaternion, Vector3};
use process_memory::{CopyAddress, ProcessHandle};
use std::convert::TryInto;
use std::io;

// Lara's (and characters') hit capsules ("DefensiveCapsule_head", ...) are listed in their CollideComponent, each
// attached to a bone of their skeleton. The TransformComponent's bone matrices (unlike its own matrix) are kept up to
// date. Where everything is is in `layout`.

#[derive(Debug, Clone)]
pub struct CapsuleShape {
    bone: usize,
    // Ends of the cylinder part, in bone space
    a: Vector3<f32>,
    b: Vector3<f32>,
    radius: f32,
}

// Shapes only change with Lara's model, so these can be read occasionally
pub fn read_capsule_shapes(
    handle: ProcessHandle,
    components: &mut Components,
    instance_address: usize,
) -> io::Result<Vec<CapsuleShape>> {
    let layout = &layout::current().collide;
    let collide = match components.get(handle, instance_address, components::COLLIDE) {
        Some(c) => c,
        None => return Ok(vec![]),
    };
    let mut header = vec![0u8; layout.shapes.max(layout.shape_count) + 8];
    handle.copy_address(collide, &mut header)?;
    let shapes = pointer_at(&header, layout.shapes);
    let count = u32_at(&header, layout.shape_count) as usize;
    if shapes == 0 || count > 256 {
        return Ok(vec![]);
    }

    let mut entries = vec![0u8; count * layout.entry_size];
    handle.copy_address(shapes, &mut entries)?;
    let mut capsules = vec![];
    for entry in entries.chunks_exact(layout.entry_size) {
        let id = u64_at(entry, layout.entry_id) as usize;
        if ((id >> 16) as usize) & 0xFF & layout.capsule_type_mask != layout.capsule_type {
            continue;
        }
        let mut shape = vec![0u8; layout.capsule_size];
        handle.copy_address(pointer_at(entry, layout.entry_shape), &mut shape)?;

        let rotation = quaternion_at(&shape, layout.capsule_rotation);
        let centre = vector_at(&shape, layout.capsule_centre);
        let half_axis = rotation * Vector3::z() * (f32_at(&shape, layout.capsule_length) / 2.0);
        capsules.push(CapsuleShape {
            bone: (id >> 32) + 1,
            a: centre - half_axis,
            b: centre + half_axis,
            radius: f32_at(&shape, layout.capsule_radius),
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
    let layout = layout::current();
    let bone_size = layout.transform.bone_size;
    let transform = match layout.instance.transform {
        Some(offset) => read_pointer_or_null(handle, instance_address + offset)?,
        None => return Ok(vec![]),
    };
    let bones_address = read_pointer_or_null(handle, transform + layout.transform.bones)?;
    if bones_address == 0 {
        return Ok(vec![]);
    }
    let mut bones = vec![0u8; bone_count * bone_size];
    handle.copy_address(bones_address, &mut bones)?;

    Ok(shapes
        .iter()
        .map(|shape| {
            let bone = &bones[shape.bone * bone_size..(shape.bone + 1) * bone_size];
            Capsule {
                a: to_world(bone, &shape.a),
                b: to_world(bone, &shape.b),
                radius: shape.radius,
            }
        })
        .collect())
}

// Lara's movement collision is the capsule of her character proxy (cdc::ProxyComponent), which the game moves her
// with. It holds the shape in use and the one it's changing to, and the physics capsule it's swept as. Other proxies'
// bodies are attached to it: the camera (radius 30, moving from 15 in front of her to 30 behind when she aims) and her
// weapon (a thin rod). Both only collide while she aims.
// How far along the body list to look for bodies attached to Lara's
const MAX_BODIES_SEARCHED: usize = 16;
const MAX_CONTACTS: usize = 8;
// Contacts this close to a capsule's surface are on it
const CONTACT_TOLERANCE: f32 = 2.0;
// Shapes this similar are drawn once
const PROXY_SAME_SHAPE: f32 = 0.01;

struct ProxyShape {
    size: usize, // Radius, then the length of the cylinder part (from the physics shape's volume)
    offset: usize, // Of the centre from Lara's feet, in her space
    local_rotation: usize, // Tilt of the capsule (e.g. lying down when rolling), quaternion x, y, z, w
    rotation: usize, // Lara's facing
}

// The shape in use, then the one it's changing to
fn proxy_shapes(layout: &layout::ProxyLayout) -> [ProxyShape; 2] {
    [
        ProxyShape {
            size: layout.in_use_size,
            offset: layout.in_use_offset,
            local_rotation: layout.in_use_local_rotation,
            rotation: layout.in_use_rotation,
        },
        ProxyShape {
            size: layout.next_size,
            offset: layout.next_offset,
            local_rotation: layout.next_local_rotation,
            rotation: layout.next_rotation,
        },
    ]
}

pub struct ProxyCapsules {
    // The one in use, then the one it's changing to if that's different
    pub shapes: Vec<Capsule>,
    // Whether the one in use is pushing her off something
    pub touching: bool,
    // Of the proxies attached to Lara's, and whether each is touching something
    pub attached: Vec<(Capsule, bool)>,
    // Whether the attached proxies collide
    pub attached_active: bool,
}

// The shapes of a proxy (Lara's cdc::ProxyComponent) in world space. `standalone` are proxies attached to it that
// aren't found through its physics body (TR2013's combat proxy).
pub fn read_proxy(handle: ProcessHandle, proxy: usize, standalone: &[usize]) -> io::Result<ProxyCapsules> {
    let layout = &layout::current().proxy;
    let shapes = proxy_shapes(layout);
    let mut bytes = vec![0u8; layout.size];
    handle.copy_address(proxy, &mut bytes)?;

    // Lara's feet plus her rotated offset is the centre for most moves, but not e.g. rolls, so use the one
    // the game stores, and place the other shape relative to it
    let mut centre = vector_at(&bytes, layout.centre);
    let in_use_offset = vector_at(&bytes, shapes[0].offset);
    let mut capsules: Vec<(Vector3<f32>, Vector3<f32>, Capsule)> = vec![];

    // The physics shape is what's actually swept, so prefer it for the shape in use
    let body = layout.body.map_or(0, |offset| pointer_at(&bytes, offset));
    if let Some((capsule, physics_centre, _)) = read_physics_capsule(handle, body) {
        let size = Vector3::new(f32_at(&bytes, shapes[0].size), f32_at(&bytes, shapes[0].size + 4), 0.0);
        capsules.push((size, in_use_offset, capsule));
        centre = physics_centre;
    }

    for shape in &shapes {
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
    let shapes: Vec<Capsule> = capsules.into_iter().map(|(_, _, capsule)| capsule).collect();

    // An attached proxy is pushing Lara if one of her contacts is on it. Its touching flag also says so,
    // unless it's just touching her own capsule (the camera does whenever she aims).
    let contacts = read_contacts(handle, proxy);
    let on_contact = |capsule: &Capsule| {
        contacts.iter().any(|c| (distance_to_capsule(c, capsule) - capsule.radius).abs() <= CONTACT_TOLERANCE)
    };
    let mut attached = if body == 0 { vec![] } else { read_attached_capsules(handle, body) };
    // They only collide while she aims, when the camera's touching flag is always set as it overlaps her capsule
    // (and the weapon's can only be set then)
    let mut attached_active = attached.iter().any(|(_, touching)| *touching);
    // Ones found on their own say whether they collide in their flags, and are only touching if on a contact
    for &other in standalone {
        if let Ok((capsule, collides)) = read_attached_proxy(handle, other) {
            attached.push((capsule, false));
            attached_active |= collides;
        }
    }
    let attached = attached
        .into_iter()
        .map(|(capsule, touching)| {
            let on_contact = on_contact(&capsule);
            let touching_lara = shapes.first().map_or(false, |lara| capsules_overlap(&capsule, lara));
            (capsule, on_contact || (touching && !touching_lara))
        })
        .collect();

    Ok(ProxyCapsules {
        // Its touching flag is always set, so only go by contacts
        touching: shapes.first().map_or(false, |capsule| on_contact(capsule)),
        shapes,
        attached,
        attached_active,
    })
}

/// A proxy attached to Lara's that's found on its own (TR2013's camera): its shape in use, and whether it collides,
/// from its flags
pub fn read_attached_proxy(handle: ProcessHandle, proxy: usize) -> io::Result<(Capsule, bool)> {
    let layout = &layout::current().proxy;
    let capsule = read_proxy(handle, proxy, &[])?
        .shapes
        .first()
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the proxy has no shapes"))?;
    // An empty slot reads as a capsule with no radius and no rotation, which comes out as NaN
    let ends = [capsule.a.x, capsule.a.y, capsule.a.z, capsule.b.x, capsule.b.y, capsule.b.z];
    if !(capsule.radius > 0.0) || !ends.iter().all(|v| v.is_finite()) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the proxy isn't in use"));
    }
    let collides = match layout.flags {
        Some(offset) => {
            let mut flags = [0u8; 4];
            handle.copy_address(proxy + offset, &mut flags)?;
            u32::from_le_bytes(flags) as usize & layout.collides != 0
        }
        None => false,
    };
    Ok((capsule, collides))
}

fn read_contacts(handle: ProcessHandle, proxy: usize) -> Vec<Vector3<f32>> {
    let layout = &layout::current().proxy;
    let contacts = match layout.contacts {
        Some(offset) => offset,
        None => return vec![],
    };
    let count_at = |offset: usize| {
        let mut bytes = [0u8; 4];
        handle.copy_address(proxy + offset, &mut bytes).ok().map(|_| i32::from_le_bytes(bytes).max(0) as usize)
    };
    let mut records = vec![0u8; MAX_CONTACTS * layout.contact_size];
    let counts = (count_at(layout.contact_count), layout.second_contact_count.map_or(Some(0), count_at));
    let count = match (counts, handle.copy_address(proxy + contacts, &mut records)) {
        ((Some(first), Some(second)), Ok(_)) => (first + second).min(MAX_CONTACTS),
        _ => return vec![],
    };
    (0..count).map(|i| vector_at(&records, i * layout.contact_size)).collect()
}

fn capsule_segment(capsule: &Capsule) -> (Vector3<f32>, Vector3<f32>) {
    (
        Vector3::new(capsule.a.x, capsule.a.y, capsule.a.z),
        Vector3::new(capsule.b.x, capsule.b.y, capsule.b.z),
    )
}

// From the point to the capsule's segment
fn distance_to_capsule(point: &Vector3<f32>, capsule: &Capsule) -> f32 {
    let (a, b) = capsule_segment(capsule);
    let line = b - a;
    let t = ((point - a).dot(&line) / line.norm_squared().max(1e-6)).clamp(0.0, 1.0);
    (point - (a + line * t)).norm()
}

fn capsules_overlap(first: &Capsule, second: &Capsule) -> bool {
    let (a, b) = capsule_segment(first);
    let (c, d) = capsule_segment(second);
    segment_distance(a, b, c, d) < first.radius + second.radius
}

// Closest distance between segments ab and cd
fn segment_distance(a: Vector3<f32>, b: Vector3<f32>, c: Vector3<f32>, d: Vector3<f32>) -> f32 {
    let (u, v, w) = (b - a, d - c, a - c);
    let (uu, uv, vv, uw, vw) = (u.dot(&u), u.dot(&v), v.dot(&v), u.dot(&w), v.dot(&w));
    let denominator = uu * vv - uv * uv;
    let mut s = if denominator > 1e-6 { ((uv * vw - vv * uw) / denominator).clamp(0.0, 1.0) } else { 0.0 };
    let t = if vv > 1e-6 { ((uv * s + vw) / vv).clamp(0.0, 1.0) } else { 0.0 };
    if uu > 1e-6 {
        s = ((uv * t - uw) / uu).clamp(0.0, 1.0);
    }
    ((a + u * s) - (c + v * t)).norm()
}

// The capsules of bodies attached (directly or not) to `body`, which follow it in the list
fn read_attached_capsules(handle: ProcessHandle, body: usize) -> Vec<(Capsule, bool)> {
    let layout = &layout::current().body;
    let mut family = vec![body];
    let mut capsules = vec![];
    let mut current = body;
    for _ in 0..MAX_BODIES_SEARCHED {
        current = match read_pointer_or_null(handle, current + layout.next) {
            Ok(next) if next != 0 && next != body => next,
            _ => break,
        };
        match read_pointer_or_null(handle, current + layout.parent) {
            Ok(parent) if family.contains(&parent) => family.push(current),
            _ => continue,
        }
        if let Some((capsule, _, touching)) = read_physics_capsule(handle, current) {
            capsules.push((capsule, touching));
        }
    }
    capsules
}

// The capsule, its centre, and whether it's touching something
fn read_physics_capsule(handle: ProcessHandle, body: usize) -> Option<(Capsule, Vector3<f32>, bool)> {
    if body == 0 {
        return None;
    }
    let all = layout::current();
    let layout = &all.physics_shape;
    let shape = read_pointer_or_null(handle, body + all.body.shape).ok()?;
    if shape == 0 {
        return None;
    }
    let mut bytes = vec![0u8; layout.size];
    handle.copy_address(shape, &mut bytes).ok()?;
    if bytes[layout.shape_type] as usize != layout.capsule_type {
        return None;
    }
    let centre = vector_at(&bytes, layout.centre);
    let radius = f32_at(&bytes, layout.radius);
    let half_axis = quaternion_at(&bytes, layout.rotation) * Vector3::z() * (f32_at(&bytes, layout.radius + 4) / 2.0);
    Some((
        Capsule { a: to_position(&(centre - half_axis)), b: to_position(&(centre + half_axis)), radius },
        centre,
        bytes[layout.touching] != 0,
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

// The ground probes the game casts for characters: for Lara one per foot (knee height to below the foot) and one for
// her body (above her head to below her feet). A static count followed by records, for every character near enough,
// so Lara's are picked out by `probes_near`.
const MAX_PROBES: usize = 64;
// Probes starting this close to a character are theirs
const PROBE_OWNER_DISTANCE: f32 = 300.0;
// Hits further than this from their probe are left over from an earlier cast
const PROBE_HIT_TOLERANCE: f32 = 10.0;

pub fn read_probes(handle: ProcessHandle, probes_address: usize) -> io::Result<Vec<Probe>> {
    let layout = &layout::current().probes;
    let mut bytes = vec![0u8; layout.records + MAX_PROBES * layout.size];
    handle.copy_address(probes_address, &mut bytes)?;
    let count = (u32_at(&bytes, 0) as usize).min(MAX_PROBES);
    Ok(bytes[layout.records..]
        .chunks_exact(layout.size)
        .take(count)
        .map(|record| {
            let start = vector_at(record, layout.start);
            let end = vector_at(record, layout.end);
            let hit = vector_at(record, layout.hit);
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

// The probes starting near the position (a character's feet)
pub fn probes_near(probes: &[Probe], position: &Position) -> Vec<Probe> {
    let position = Vector3::new(position.x, position.y, position.z);
    probes
        .iter()
        .filter(|p| (Vector3::new(p.start.x, p.start.y, p.start.z) - position).norm() < PROBE_OWNER_DISTANCE)
        .copied()
        .collect()
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





#[cfg(test)]
mod tests {
    use super::*;

    fn capsule(a: (f32, f32, f32), b: (f32, f32, f32), radius: f32) -> Capsule {
        Capsule { a: Position { x: a.0, y: a.1, z: a.2 }, b: Position { x: b.0, y: b.1, z: b.2 }, radius }
    }

    #[test]
    fn segment_distance_handles_parallel_crossing_and_ends() {
        let v = |x: f32, y: f32, z: f32| Vector3::new(x, y, z);
        assert!((segment_distance(v(0., 0., 0.), v(0., 0., 10.), v(5., 0., 0.), v(5., 0., 10.)) - 5.0).abs() < 1e-4);
        assert!(segment_distance(v(-1., 0., 0.), v(1., 0., 0.), v(0., -1., 0.), v(0., 1., 0.)) < 1e-4);
        assert!((segment_distance(v(0., 0., 0.), v(1., 0., 0.), v(3., 0., 0.), v(4., 0., 0.)) - 2.0).abs() < 1e-4);
    }

    #[test]
    fn aiming_camera_overlaps_laras_capsule_but_not_the_weapon() {
        // Positions relative to Lara's feet while aiming
        let lara = capsule((0., -10., 70.), (0., -10., 110.), 25.);
        let camera = capsule((0., 30., 75.), (0., 30., 105.), 30.);
        let weapon = capsule((-16., -102., 125.), (-16., -12., 125.), 3.5);
        assert!(capsules_overlap(&camera, &lara));
        assert!(!capsules_overlap(&capsule((0., 80., 75.), (0., 80., 105.), 30.), &lara));
        assert!((distance_to_capsule(&Vector3::new(0., 60., 90.), &camera) - camera.radius).abs() < 1e-4);
        assert!(distance_to_capsule(&Vector3::new(0., -10., 90.), &weapon) > weapon.radius);
    }
}
