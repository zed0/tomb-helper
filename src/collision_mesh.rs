use crate::game_memory::{Handle, pointer_at, read_u32, u16_at, u32_at, vector_at};
use crate::layout;
use crate::position::Position;
use crate::ui_ipc_data::{SurfaceKind, Triangle};
use nalgebra::Vector3;
use process_memory::CopyAddress;
use std::collections::{HashMap, HashSet};
use std::io;

// The level's collision is triangle meshes placed in the world by instances, which the cdc::CollisionSceneClient
// (`CollisionScene`) keeps in a list. It's the ground and everything fixed in the level (walls, rocks, buildings);
// physics props are separate bodies. Found in ROTTR (Steam) from the code testing a mesh's triangles (+0x18C5F0).
// Where things are is in `layout`. Each instance's filter has bits for which kinds of collision use the mesh. Everything
// Lara's movement proxy touched had the player bit set (as did the level's default, 0x2C7F), and objects often have
// both a detailed mesh without it (0x1C10, probably for projectiles) and a rough one with it (0x204E). About 900 meshes
// are just 0x0002, presumably invisible walls. So the bits are kinds of collision (channels), not levels of detail: the
// ground is one mesh with nearly every bit set, used by everything. SOTTR's kinds are mostly ROTTR's plus 0x4000.
// In case the list is broken
const MAX_INSTANCES: usize = 100_000;
// Normals with at least this much z are floors, at most minus this ceilings
const FLOOR_NORMAL_Z: f32 = 0.7;
// Instances are only reread each tick if they were this much closer than the radius when the list was last read, which
// leaves room for Lara and them moving in between
const NEARBY_MARGIN: f32 = 1500.0;
// Instances are tested against a ray if it passes this close to where they were when the list was last read
const RAY_MARGIN: f32 = 100.0;

struct Placement {
    address: usize,
    mesh: usize,
    flipped: bool,
    detailed: bool,
    // World space bounds when the list was read
    min: Vector3<f32>,
    max: Vector3<f32>,
    // Its triangles in world space, and the matrix they were placed with
    placed: Option<(Vec<u8>, Vec<PlacedTriangle>)>,
}

struct PlacedTriangle {
    min: Vector3<f32>,
    max: Vector3<f32>,
    triangle: Triangle,
}

pub struct Mesh {
    // To notice the mesh being replaced by another at the same address
    header: Vec<u8>,
    pub vertices: Vec<Vector3<f32>>,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Default)]
pub struct CollisionMeshes {
    placements: Vec<Placement>,
    meshes: HashMap<usize, Mesh>,
    /// How many of the level's instances have each of the 16 kind bits, as of the last refresh
    pub kind_counts: [u32; 16],
}

impl CollisionMeshes {
    // Rereads the list of instances, which changes as the level streams in and out. Keeps those Lara collides with if
    // `coarse`, and those with any of `other_kinds` if `detailed` (which are "detailed" in the placements unless Lara
    // collides with them and `coarse`).
    pub fn refresh(
        &mut self,
        handle: Handle,
        scene_address: usize,
        coarse: bool,
        detailed: bool,
        other_kinds: u32,
    ) -> io::Result<()> {
        let layout = layout::current();
        let (scene_layout, instance) = (&layout.collision_scene, &layout.collision_instance);
        let mesh_layout = &layout.collision_mesh;
        let mut scene = vec![0u8; scene_layout.first.max(scene_layout.count) + 8];
        handle.copy_address(scene_address, &mut scene)?;
        let count = (u32_at(&scene, scene_layout.count) as usize).min(MAX_INSTANCES);

        // Keep what's already been placed
        let mut old: HashMap<(usize, usize), Placement> =
            self.placements.drain(..).map(|p| ((p.address, p.mesh), p)).collect();
        let mut headers = HashMap::new();
        let mut filters = HashMap::new();
        let mut placements: Vec<Placement> = vec![];
        // Many meshes are loaded twice and placed in the same place, with different kinds; the index of the one kept
        let mut seen = HashMap::new();
        let mut kind_counts = [0u32; 16];
        let mut address = pointer_at(&scene, scene_layout.first);
        let mut bytes = vec![0u8; instance.size];
        let mut visited = 0;
        while address != 0 && visited < count {
            visited += 1;
            handle.copy_address(address, &mut bytes)?;
            let next = pointer_at(&bytes, instance.next);
            // Without a filter (TR2013), every mesh counts as Lara's
            let kinds = match instance.filter {
                Some(offset) => {
                    let filter = pointer_at(&bytes, offset);
                    if !filters.contains_key(&filter) {
                        let kinds = read_u32(handle, filter + instance.filter_kinds)
                            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "unreadable collision filter"))?;
                        filters.insert(filter, kinds as usize);
                    }
                    filters[&filter]
                }
                None => instance.player_kind,
            };
            for (bit, count) in kind_counts.iter_mut().enumerate() {
                *count += (kinds >> bit & 1) as u32;
            }
            let for_lara = coarse && kinds & instance.player_kind != 0;
            if !for_lara && !(detailed && kinds & other_kinds as usize != 0) {
                address = next;
                continue;
            }
            let is_detailed = !for_lara;
            let mesh = pointer_at(&bytes, instance.mesh);
            if !headers.contains_key(&mesh) {
                headers.insert(mesh, self.mesh_header(handle, mesh)?);
            }
            let header = &headers[&mesh];
            let counts = mesh_layout.triangle_count.min(header.len())..header.len();
            let key = (bytes[..instance.placement_size].to_vec(), header[counts].to_vec());
            if let Some(&kept) = seen.get(&key) {
                // Drawn as Lara's if either copy is
                let kept: &mut Placement = &mut placements[kept];
                kept.detailed &= is_detailed;
            } else {
                seen.insert(key, placements.len());
                // Unless it was placed with a mesh that's since been replaced
                let placed = old
                    .remove(&(address, mesh))
                    .and_then(|p| p.placed)
                    .filter(|_| self.meshes.contains_key(&mesh));
                placements.push(Placement {
                    address,
                    mesh,
                    flipped: instance.flags.map_or(false, |offset| u16_at(&bytes, offset) & 1 != 0),
                    detailed: is_detailed,
                    min: vector_at(&bytes, instance.min) + origin(&bytes),
                    max: vector_at(&bytes, instance.max) + origin(&bytes),
                    placed,
                });
            }
            address = next;
        }

        let used: HashSet<usize> = placements.iter().map(|p| p.mesh).collect();
        self.meshes.retain(|address, _| used.contains(address));
        self.placements = placements;
        self.kind_counts = kind_counts;
        Ok(())
    }

    // Drops the cached mesh if the header changed, so it gets loaded again when it's needed
    fn mesh_header(&mut self, handle: Handle, address: usize) -> io::Result<Vec<u8>> {
        let mut header = vec![0u8; layout::current().collision_mesh.header_size];
        handle.copy_address(address, &mut header)?;
        if self.meshes.get(&address).map_or(false, |m| m.header != header) {
            self.meshes.remove(&address);
        }
        Ok(header)
    }

    // The triangles that come within the radius for their kind of mesh of `centre`, closest first
    pub fn triangles_near(
        &mut self,
        handle: Handle,
        centre: &Position,
        coarse_radius: f32,
        detailed_radius: f32,
        max: usize,
    ) -> Vec<Triangle> {
        let layout = &layout::current().collision_instance;
        let centre = centre.vector();
        let mut found = vec![];
        for placement in &mut self.placements {
            let radius = if placement.detailed { detailed_radius } else { coarse_radius };
            if box_distance(&centre, &placement.min, &placement.max) > radius + NEARBY_MARGIN {
                continue;
            }
            // Instances can move (e.g. bendy trees) or be unloaded between refreshes
            let mut bytes = vec![0u8; layout.placement_size];
            if handle.copy_address(placement.address, &mut bytes).is_err() {
                continue;
            }
            let (min, max) = (vector_at(&bytes, layout.min) + origin(&bytes), vector_at(&bytes, layout.max) + origin(&bytes));
            if box_distance(&centre, &min, &max) > radius || !place(handle, placement, &mut self.meshes, &bytes) {
                continue;
            }
            for placed in &placement.placed.as_ref().unwrap().1 {
                let distance = box_distance(&centre, &placed.min, &placed.max);
                if distance <= radius {
                    found.push((distance, placed.triangle));
                }
            }
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().take(max).map(|(_, t)| t).collect()
    }
}

impl CollisionMeshes {
    /// How far along the ray from `origin` towards `direction` (unit length) it first hits one of the meshes, if it does
    /// within `max`
    pub fn raycast(&mut self, handle: Handle, origin: Vector3<f32>, direction: Vector3<f32>, max: f32) -> Option<f32> {
        let layout = &layout::current().collision_instance;
        let margin = Vector3::repeat(RAY_MARGIN);
        let mut nearest: Option<f32> = None;
        for placement in &mut self.placements {
            let far = nearest.unwrap_or(max);
            if !ray_hits_box(&origin, &direction, far, &(placement.min - margin), &(placement.max + margin)) {
                continue;
            }
            let mut bytes = vec![0u8; layout.placement_size];
            if handle.copy_address(placement.address, &mut bytes).is_err() || !place(handle, placement, &mut self.meshes, &bytes) {
                continue;
            }
            for placed in &placement.placed.as_ref().unwrap().1 {
                if let Some(t) = ray_triangle(&origin, &direction, &placed.triangle) {
                    if t < nearest.unwrap_or(max) {
                        nearest = Some(t);
                    }
                }
            }
        }
        nearest
    }
}

// Places the placement's mesh in the world with the matrix in `bytes` (the instance as it is now), loading it if need
// be; false if it can't be read
fn place(handle: Handle, placement: &mut Placement, meshes: &mut HashMap<usize, Mesh>, bytes: &[u8]) -> bool {
    let layout = &layout::current().collision_instance;
    if !meshes.contains_key(&placement.mesh) {
        match read_mesh(handle, placement.mesh) {
            Ok(mesh) => meshes.insert(placement.mesh, mesh),
            Err(_) => return false,
        };
        placement.placed = None;
    }
    let mut matrix = bytes[layout.matrix..layout.matrix + 0x40].to_vec();
    // The origin goes on the translation (the last row)
    let translation = vector_at(&matrix, 48) + origin(bytes);
    for axis in 0..3 {
        matrix[48 + axis * 4..52 + axis * 4].copy_from_slice(&translation[axis].to_le_bytes());
    }
    if placement.placed.as_ref().map_or(true, |(m, _)| *m != matrix) {
        let triangles = place_triangles(&meshes[&placement.mesh], &matrix, placement.flipped, placement.detailed);
        placement.placed = Some((matrix, triangles));
    }
    true
}

// What the instance's vertices are relative to, after its matrix (TR2013's level meshes), or none
fn origin(bytes: &[u8]) -> Vector3<f32> {
    layout::current().collision_instance.origin.map_or(Vector3::zeros(), |offset| vector_at(bytes, offset))
}

// Whether the ray reaches the box within `max`
fn ray_hits_box(origin: &Vector3<f32>, direction: &Vector3<f32>, max: f32, min: &Vector3<f32>, top: &Vector3<f32>) -> bool {
    let (mut near, mut far) = (0.0f32, max);
    for axis in 0..3 {
        if direction[axis].abs() < 1e-9 {
            if origin[axis] < min[axis] || origin[axis] > top[axis] {
                return false;
            }
            continue;
        }
        let (a, b) = ((min[axis] - origin[axis]) / direction[axis], (top[axis] - origin[axis]) / direction[axis]);
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return false;
        }
    }
    true
}

// How far along the ray it hits the triangle (from either side), by Moller-Trumbore
fn ray_triangle(origin: &Vector3<f32>, direction: &Vector3<f32>, triangle: &Triangle) -> Option<f32> {
    let [a, b, c] = triangle.corners.map(|p| p.vector());
    let (ab, ac) = (b - a, c - a);
    let p = direction.cross(&ac);
    let determinant = ab.dot(&p);
    if determinant.abs() < 1e-9 {
        return None;
    }
    let to_origin = origin - a;
    let u = to_origin.dot(&p) / determinant;
    let q = to_origin.cross(&ab);
    let v = direction.dot(&q) / determinant;
    let t = ac.dot(&q) / determinant;
    if u < 0.0 || v < 0.0 || u + v > 1.0 || t < 0.0 {
        return None;
    }
    Some(t)
}

pub fn read_mesh(handle: Handle, address: usize) -> io::Result<Mesh> {
    let layout = &layout::current().collision_mesh;
    let mut header = vec![0u8; layout.header_size];
    handle.copy_address(address, &mut header)?;
    let vertex_count = u32_at(&header, layout.vertex_count) as usize;
    let triangle_count = u32_at(&header, layout.triangle_count) as usize;

    let format = u16_at(&header, layout.vertex_format) as usize;
    let vertices = if format == layout.format_float {
        let mut bytes = vec![0u8; vertex_count * 12];
        handle.copy_address(pointer_at(&header, layout.vertices), &mut bytes)?;
        bytes.chunks_exact(12).map(|v| vector_at(v, 0)).collect()
    } else if format == layout.format_half {
        let mut bytes = vec![0u8; vertex_count * 6];
        handle.copy_address(pointer_at(&header, layout.vertices), &mut bytes)?;
        bytes
            .chunks_exact(6)
            .map(|v| Vector3::new(half_at(v, 0), half_at(v, 2), half_at(v, 4)))
            .collect()
    } else {
        // Not seen, and the game just copies 6 bytes, so who knows
        vec![]
    };

    let mut bytes = vec![0u8; triangle_count * layout.triangle_size];
    handle.copy_address(pointer_at(&header, layout.triangles), &mut bytes)?;
    let triangles = bytes
        .chunks_exact(layout.triangle_size)
        .filter(|t| t[layout.triangle_flags] as usize & layout.triangle_ignored == 0)
        .map(|t| [u32_at(t, 0), u32_at(t, 4), u32_at(t, 8)])
        .filter(|t| t.iter().all(|&i| (i as usize) < vertices.len()))
        .collect();

    Ok(Mesh { header, vertices, triangles })
}

// The mesh's triangles in world space
fn place_triangles(mesh: &Mesh, matrix: &[u8], flipped: bool, detailed: bool) -> Vec<PlacedTriangle> {
    // `point * matrix` for the game's row-major matrices of row vectors
    let rows = [vector_at(matrix, 0), vector_at(matrix, 16), vector_at(matrix, 32), vector_at(matrix, 48)];
    let world: Vec<Vector3<f32>> = mesh
        .vertices
        .iter()
        .map(|v| rows[0] * v.x + rows[1] * v.y + rows[2] * v.z + rows[3])
        .collect();

    mesh.triangles
        .iter()
        .map(|indices| {
            let mut corners = indices.map(|i| world[i as usize]);
            if flipped {
                corners.swap(0, 1);
            }
            PlacedTriangle {
                min: corners[0].inf(&corners[1]).inf(&corners[2]),
                max: corners[0].sup(&corners[1]).sup(&corners[2]),
                triangle: Triangle {
                    corners: corners.map(Position::from),
                    kind: surface_kind(&corners),
                    detailed,
                },
            }
        })
        .collect()
}

fn surface_kind(corners: &[Vector3<f32>; 3]) -> SurfaceKind {
    let normal = (corners[1] - corners[0]).cross(&(corners[2] - corners[0]));
    let up = normal.z / normal.norm().max(1e-9);
    if up >= FLOOR_NORMAL_Z {
        SurfaceKind::Floor
    } else if up <= -FLOOR_NORMAL_Z {
        SurfaceKind::Ceiling
    } else {
        SurfaceKind::Wall
    }
}

// Distance from the point to the box, 0 inside it
fn box_distance(point: &Vector3<f32>, min: &Vector3<f32>, max: &Vector3<f32>) -> f32 {
    (min - point).sup(&(point - max)).sup(&Vector3::zeros()).norm()
}






// IEEE half precision, without infinities or NaNs (like the game's conversion)
fn half_at(bytes: &[u8], offset: usize) -> f32 {
    let bits = u16_at(bytes, offset) as u32;
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1F) as i32;
    let mantissa = (bits & 0x3FF) as f32;
    if exponent == 0 {
        sign * mantissa * 2f32.powi(-24)
    } else {
        sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rays_hit_triangles_and_boxes() {
        let p = |x, y, z| Position { x, y, z };
        let triangle = Triangle { corners: [p(0.0, 0.0, 0.0), p(10.0, 0.0, 0.0), p(0.0, 10.0, 0.0)], kind: SurfaceKind::Floor, detailed: false };
        let down = Vector3::new(0.0, 0.0, -1.0);
        assert_eq!(ray_triangle(&Vector3::new(1.0, 1.0, 5.0), &down, &triangle), Some(5.0));
        // From below too, but not past its edge or behind the ray
        assert_eq!(ray_triangle(&Vector3::new(1.0, 1.0, -5.0), &-down, &triangle), Some(5.0));
        assert_eq!(ray_triangle(&Vector3::new(8.0, 8.0, 5.0), &down, &triangle), None);
        assert_eq!(ray_triangle(&Vector3::new(1.0, 1.0, 5.0), &-down, &triangle), None);
        let (min, max) = (Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0));
        assert!(ray_hits_box(&Vector3::new(0.5, 0.5, 5.0), &down, 10.0, &min, &max));
        assert!(!ray_hits_box(&Vector3::new(0.5, 0.5, 5.0), &down, 3.0, &min, &max));
        assert!(!ray_hits_box(&Vector3::new(2.0, 0.5, 5.0), &down, 10.0, &min, &max));
    }

    fn matrix_bytes(rows: [[f32; 4]; 4]) -> Vec<u8> {
        rows.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn square_mesh() -> Mesh {
        Mesh {
            header: vec![],
            vertices: vec![
                Vector3::new(0., 0., 0.),
                Vector3::new(10., 0., 0.),
                Vector3::new(0., 10., 0.),
                Vector3::new(1000., 1000., 0.),
                Vector3::new(1010., 1000., 0.),
                Vector3::new(1000., 1010., 0.),
            ],
            triangles: vec![[0, 1, 2], [3, 4, 5]],
        }
    }

    #[test]
    fn halves_decode() {
        let half = |bits: u16| half_at(&bits.to_le_bytes(), 0);
        assert_eq!(half(0x3C00), 1.0);
        assert_eq!(half(0xC000), -2.0);
        assert_eq!(half(0x5640), 100.0);
        assert_eq!(half(0x0000), 0.0);
        assert_eq!(half(0x0001), 2f32.powi(-24));
    }

    #[test]
    fn triangles_are_placed_in_the_world() {
        // Rotated 90 degrees about x (y goes to z) and moved up 100
        let matrix = matrix_bytes([
            [1., 0., 0., 0.],
            [0., 0., 1., 0.],
            [0., -1., 0., 0.],
            [0., 0., 100., 1.],
        ]);
        let placed = place_triangles(&square_mesh(), &matrix, false, true);
        let triangle = &placed[0];
        assert!((box_distance(&Vector3::new(0., -20., 105.), &triangle.min, &triangle.max) - 20.).abs() < 1e-4);
        assert_eq!(triangle.triangle.corners[2].z, 110.);
        assert_eq!(triangle.triangle.kind, SurfaceKind::Wall);
        assert!(triangle.triangle.detailed);
        assert_eq!(placed[1].min, Vector3::new(1000., 0., 1100.));
    }

    #[test]
    fn winding_gives_the_kind_and_flipping_reverses_it() {
        let identity = matrix_bytes([[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.], [0., 0., 0., 1.]]);
        let kind = |flipped| place_triangles(&square_mesh(), &identity, flipped, false)[0].triangle.kind;
        assert_eq!(kind(false), SurfaceKind::Floor);
        assert_eq!(kind(true), SurfaceKind::Ceiling);
    }
}
