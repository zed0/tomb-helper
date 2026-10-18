use crate::position::Position;
use crate::ui_ipc_data::{SurfaceKind, Triangle};
use nalgebra::Vector3;
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;

// The level's collision is triangle meshes placed in the world by instances, which the cdc::CollisionSceneClient
// (`CollisionScene`) keeps in a list. It's the ground and everything fixed in the level (walls, rocks, buildings);
// physics props are separate bodies. Found in ROTTR (Steam) from the code testing a mesh's triangles (+0x18C5F0).
const SCENE_FIRST: usize = 0x28; // -> first instance
const SCENE_COUNT: usize = 0x30; // u32
const INSTANCE_SIZE: usize = 0x98;
const INSTANCE_MATRIX: usize = 0x0; // Row-major object-to-world matrix (row vectors)
const INSTANCE_MIN: usize = 0x40; // World space bounds
const INSTANCE_MAX: usize = 0x50;
// The matrix and bounds, which is what's reread each tick
const INSTANCE_PLACEMENT_SIZE: usize = 0x60;
const INSTANCE_MESH: usize = 0x70;
const INSTANCE_NEXT: usize = 0x88;
const INSTANCE_FLAGS: usize = 0x90; // u16, bit 0 swaps the first two corners of each triangle
const MESH_HEADER_SIZE: usize = 0x48;
const MESH_VERTICES: usize = 0x10;
const MESH_TRIANGLES: usize = 0x18;
const MESH_TRIANGLE_COUNT: usize = 0x34; // u32
const MESH_VERTEX_COUNT: usize = 0x38; // u32
const MESH_VERTEX_FORMAT: usize = 0x44; // u16
const FORMAT_FLOAT: u16 = 1; // 3 f32s
const FORMAT_HALF: u16 = 2; // 3 f16s
const TRIANGLE_SIZE: usize = 0x10; // 3 u32 vertex indices, a flags byte and a material byte
const TRIANGLE_FLAGS: usize = 0xC;
const TRIANGLE_IGNORED: u8 = 0x40; // The game's collision tests skip these
// In case the list is broken
const MAX_INSTANCES: usize = 100_000;
// Normals with at least this much z are floors, at most minus this ceilings
const FLOOR_NORMAL_Z: f32 = 0.7;
// Instances are only reread each tick if they were this much closer than the radius when the list was last read, which
// leaves room for Lara and them moving in between
const NEARBY_MARGIN: f32 = 1500.0;

struct Placement {
    address: usize,
    mesh: usize,
    flipped: bool,
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

struct Mesh {
    // To notice the mesh being replaced by another at the same address
    header: Vec<u8>,
    vertices: Vec<Vector3<f32>>,
    triangles: Vec<[u32; 3]>,
}

#[derive(Default)]
pub struct CollisionMeshes {
    placements: Vec<Placement>,
    meshes: HashMap<usize, Mesh>,
}

impl CollisionMeshes {
    // Rereads the list of instances, which changes as the level streams in and out
    pub fn refresh(&mut self, handle: ProcessHandle, scene_address: usize) -> io::Result<()> {
        let mut scene = [0u8; SCENE_COUNT + 4];
        handle.copy_address(scene_address, &mut scene)?;
        let count = (u32_at(&scene, SCENE_COUNT) as usize).min(MAX_INSTANCES);

        // Keep what's already been placed
        let mut old: HashMap<(usize, usize), Placement> =
            self.placements.drain(..).map(|p| ((p.address, p.mesh), p)).collect();
        let mut headers = HashMap::new();
        let mut placements = vec![];
        // Many meshes are loaded twice and placed in the same place
        let mut seen = HashSet::new();
        let mut address = usize_at(&scene, SCENE_FIRST);
        let mut bytes = [0u8; INSTANCE_SIZE];
        let mut visited = 0;
        while address != 0 && visited < count {
            visited += 1;
            handle.copy_address(address, &mut bytes)?;
            let mesh = usize_at(&bytes, INSTANCE_MESH);
            if !headers.contains_key(&mesh) {
                headers.insert(mesh, self.mesh_header(handle, mesh)?);
            }
            let header = &headers[&mesh];
            let key = (bytes[..INSTANCE_PLACEMENT_SIZE].to_vec(), header[MESH_TRIANGLE_COUNT..MESH_HEADER_SIZE].to_vec());
            if seen.insert(key) {
                // Unless it was placed with a mesh that's since been replaced
                let placed = old
                    .remove(&(address, mesh))
                    .and_then(|p| p.placed)
                    .filter(|_| self.meshes.contains_key(&mesh));
                placements.push(Placement {
                    address,
                    mesh,
                    flipped: u16_at(&bytes, INSTANCE_FLAGS) & 1 != 0,
                    min: vector_at(&bytes, INSTANCE_MIN),
                    max: vector_at(&bytes, INSTANCE_MAX),
                    placed,
                });
            }
            address = usize_at(&bytes, INSTANCE_NEXT);
        }

        let used: HashSet<usize> = placements.iter().map(|p| p.mesh).collect();
        self.meshes.retain(|address, _| used.contains(address));
        self.placements = placements;
        Ok(())
    }

    // Drops the cached mesh if the header changed, so it gets loaded again when it's needed
    fn mesh_header(&mut self, handle: ProcessHandle, address: usize) -> io::Result<Vec<u8>> {
        let mut header = vec![0u8; MESH_HEADER_SIZE];
        handle.copy_address(address, &mut header)?;
        if self.meshes.get(&address).map_or(false, |m| m.header != header) {
            self.meshes.remove(&address);
        }
        Ok(header)
    }

    // The triangles that come within `radius` of `centre`, closest first
    pub fn triangles_near(&mut self, handle: ProcessHandle, centre: &Position, radius: f32, max: usize) -> Vec<Triangle> {
        let centre = Vector3::new(centre.x, centre.y, centre.z);
        let mut found = vec![];
        for placement in &mut self.placements {
            if box_distance(&centre, &placement.min, &placement.max) > radius + NEARBY_MARGIN {
                continue;
            }
            // Instances can move (e.g. bendy trees) or be unloaded between refreshes
            let mut bytes = [0u8; INSTANCE_PLACEMENT_SIZE];
            if handle.copy_address(placement.address, &mut bytes).is_err() {
                continue;
            }
            let (min, max) = (vector_at(&bytes, INSTANCE_MIN), vector_at(&bytes, INSTANCE_MAX));
            if box_distance(&centre, &min, &max) > radius {
                continue;
            }
            if !self.meshes.contains_key(&placement.mesh) {
                match read_mesh(handle, placement.mesh) {
                    Ok(mesh) => self.meshes.insert(placement.mesh, mesh),
                    Err(_) => continue,
                };
                placement.placed = None;
            }
            let matrix = &bytes[INSTANCE_MATRIX..INSTANCE_MATRIX + 0x40];
            if placement.placed.as_ref().map_or(true, |(m, _)| m.as_slice() != matrix) {
                let triangles = place_triangles(&self.meshes[&placement.mesh], matrix, placement.flipped);
                placement.placed = Some((matrix.to_vec(), triangles));
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

fn read_mesh(handle: ProcessHandle, address: usize) -> io::Result<Mesh> {
    let mut header = vec![0u8; MESH_HEADER_SIZE];
    handle.copy_address(address, &mut header)?;
    let vertex_count = u32_at(&header, MESH_VERTEX_COUNT) as usize;
    let triangle_count = u32_at(&header, MESH_TRIANGLE_COUNT) as usize;

    let vertices = match u16_at(&header, MESH_VERTEX_FORMAT) {
        FORMAT_FLOAT => {
            let mut bytes = vec![0u8; vertex_count * 12];
            handle.copy_address(usize_at(&header, MESH_VERTICES), &mut bytes)?;
            bytes.chunks_exact(12).map(|v| vector_at(v, 0)).collect()
        }
        FORMAT_HALF => {
            let mut bytes = vec![0u8; vertex_count * 6];
            handle.copy_address(usize_at(&header, MESH_VERTICES), &mut bytes)?;
            bytes
                .chunks_exact(6)
                .map(|v| Vector3::new(half_at(v, 0), half_at(v, 2), half_at(v, 4)))
                .collect()
        }
        // Not seen, and the game just copies 6 bytes, so who knows
        _ => vec![],
    };

    let mut bytes = vec![0u8; triangle_count * TRIANGLE_SIZE];
    handle.copy_address(usize_at(&header, MESH_TRIANGLES), &mut bytes)?;
    let triangles = bytes
        .chunks_exact(TRIANGLE_SIZE)
        .filter(|t| t[TRIANGLE_FLAGS] & TRIANGLE_IGNORED == 0)
        .map(|t| [u32_at(t, 0), u32_at(t, 4), u32_at(t, 8)])
        .filter(|t| t.iter().all(|&i| (i as usize) < vertices.len()))
        .collect();

    Ok(Mesh { header, vertices, triangles })
}

// The mesh's triangles in world space
fn place_triangles(mesh: &Mesh, matrix: &[u8], flipped: bool) -> Vec<PlacedTriangle> {
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
                    corners: corners.map(|c| Position { x: c.x, y: c.y, z: c.z }),
                    kind: surface_kind(&corners),
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

fn usize_at(bytes: &[u8], offset: usize) -> usize {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn vector_at(bytes: &[u8], offset: usize) -> Vector3<f32> {
    Vector3::new(f32_at(bytes, offset), f32_at(bytes, offset + 4), f32_at(bytes, offset + 8))
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
        let placed = place_triangles(&square_mesh(), &matrix, false);
        let triangle = &placed[0];
        assert!((box_distance(&Vector3::new(0., -20., 105.), &triangle.min, &triangle.max) - 20.).abs() < 1e-4);
        assert_eq!(triangle.triangle.corners[2].z, 110.);
        assert_eq!(triangle.triangle.kind, SurfaceKind::Wall);
        assert_eq!(placed[1].min, Vector3::new(1000., 0., 1100.));
    }

    #[test]
    fn winding_gives_the_kind_and_flipping_reverses_it() {
        let identity = matrix_bytes([[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.], [0., 0., 0., 1.]]);
        let kind = |flipped| place_triangles(&square_mesh(), &identity, flipped)[0].triangle.kind;
        assert_eq!(kind(false), SurfaceKind::Floor);
        assert_eq!(kind(true), SurfaceKind::Ceiling);
    }
}
