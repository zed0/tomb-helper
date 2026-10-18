use std::ptr;
use std::sync::atomic::{fence, AtomicUsize, Ordering};
use shared_memory::{Shmem, ShmemConf, ShmemError};
use crate::position::Position;

pub const MAX_OBJECTS: usize = 64;
pub const MAX_CAPSULES: usize = 32;
pub const MAX_PROBES: usize = 8;
pub const MAX_PROXIES: usize = 2;
pub const MAX_ATTACHED: usize = 8;
pub const OBJECT_NAME_LENGTH: usize = 64;
pub const MAX_TRIANGLES: usize = 16384;

#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoxKind {
    // The object has no model, so the box is just `object_box_size`
    Estimated,
    // The bounds of the object's model
    Model,
    Lara,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ObjectBox {
    // Row-major object-to-world matrix as stored by the game (row vectors)
    pub transform: [f32; 16],
    // Box corners in object space
    pub min: Position,
    pub max: Position,
    pub kind: BoxKind,
    // Nul padded placement name
    pub name: [u8; OBJECT_NAME_LENGTH],
}

impl ObjectBox {
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        std::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

// World space ends of the cylinder part, and its radius
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Capsule {
    pub a: Position,
    pub b: Position,
    pub radius: f32,
}

// A ray the game casts for Lara's movement, and where it hit
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Probe {
    pub start: Position,
    pub end: Position,
    pub hit: Position,
    pub has_hit: bool,
}

#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    // Facing up enough to stand on
    Floor,
    Wall,
    // Facing down
    Ceiling,
}

// A world space triangle of the level's collision, wound anticlockwise seen from the side it faces
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Triangle {
    pub corners: [Position; 3],
    pub kind: SurfaceKind,
}

// All zeros is a valid (empty) value, which `zeroed` relies on
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct UiData {
    pub debug: bool,
    pub should_exit: bool,
    pub position: Position,
    // Row-major world-to-view and projection matrices as stored by the game (row vectors)
    pub camera_view: [f32; 16],
    pub camera_projection: [f32; 16],
    pub object_count: usize,
    pub objects: [ObjectBox; MAX_OBJECTS],
    // Lara's collision
    pub capsule_count: usize,
    pub capsules: [Capsule; MAX_CAPSULES],
    pub probe_count: usize,
    pub probes: [Probe; MAX_PROBES],
    // Lara's movement collision: her proxy's shape, and the one it's changing to if that's different
    pub proxy_count: usize,
    pub proxies: [Capsule; MAX_PROXIES],
    // Whether the shape in use is pushing her off something
    pub proxy_touching: bool,
    // Proxies attached to Lara's, e.g. the one that moves behind her when she aims
    pub attached_count: usize,
    pub attached: [Capsule; MAX_ATTACHED],
    pub attached_touching: [bool; MAX_ATTACHED],
    // They only collide while Lara aims
    pub attached_active: bool,
    // The level's collision near Lara, closest first
    pub triangle_count: usize,
    // How long the exe took to find them
    pub triangle_milliseconds: f32,
    pub triangles: [Triangle; MAX_TRIANGLES],
}

impl UiData {
    // Too big to build on the stack (especially the game's render thread's), so it's kept boxed
    pub fn zeroed() -> Box<UiData> {
        let layout = std::alloc::Layout::new::<UiData>();
        unsafe {
            let data = std::alloc::alloc_zeroed(layout) as *mut UiData;
            if data.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(data)
        }
    }
}

#[repr(C)]
pub struct UiDoubleBuffer {
    active_buffer: AtomicUsize,
    // Odd while the matching buffer is being written
    sequences: [AtomicUsize; 2],
    buffers: [UiData; 2],
}

impl UiDoubleBuffer {
    // Copies the current value into the other buffer, changes it there, then makes that one current. Done in
    // place since `UiData` is too big to pass around by value.
    pub fn update(&mut self, change: impl FnOnce(&mut UiData)) {
        let current = self.active_buffer.load(Ordering::Relaxed);
        let next = 1 - current;
        self.sequences[next].fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        let buffers = self.buffers.as_mut_ptr();
        unsafe {
            ptr::copy_nonoverlapping(buffers.add(current), buffers.add(next), 1);
            change(&mut *buffers.add(next));
        }
        self.sequences[next].fetch_add(1, Ordering::Release);
        self.active_buffer.store(next, Ordering::Release);
    }

    // The writer is in another process and doesn't wait for readers, so it can start rewriting a buffer
    // while it's being copied. Returns false if that happened, in which case `out` is left part written.
    pub fn try_read(&self, out: &mut UiData) -> bool {
        let current = self.active_buffer.load(Ordering::Acquire);
        let before = self.sequences[current].load(Ordering::Acquire);
        if before % 2 == 1 {
            return false;
        }
        unsafe { ptr::copy_nonoverlapping(&self.buffers[current], out, 1) };
        fence(Ordering::Acquire);
        self.sequences[current].load(Ordering::Relaxed) == before
    }

    pub fn read(&self, out: &mut UiData) {
        while !self.try_read(out) {
            std::hint::spin_loop();
        }
    }
}

pub struct SharedMemoryManager {
    shmem: Shmem,
    shared_ptr: *mut UiDoubleBuffer,
}

impl SharedMemoryManager {
    pub fn new(is_owner: bool) -> Self {
        // The mapping is backed by a file that outlives a crashed exe, so include the layout's size in the
        // name to never open one left behind by a build with a different `UiData`
        let size = size_of::<UiDoubleBuffer>();
        let os_id = format!("TombHelperIPC-{}", size);
        let mut shmem = match ShmemConf::new().size(size).os_id(&os_id).create() {
            Ok(m) => m,
            Err(ShmemError::MappingIdExists) => ShmemConf::new().os_id(&os_id).open().unwrap(),
            Err(e) => panic!("Unable to open IPC! {}", e),
        };
        assert!(shmem.len() >= size, "IPC mapping {} is only {} bytes, expected {}", os_id, shmem.len(), size);
        shmem.set_owner(is_owner);

        let shared_ptr = shmem.as_ptr() as *mut UiDoubleBuffer;

        SharedMemoryManager{
            shmem,
            shared_ptr
        }
    }

    pub fn get_mut(&mut self) -> &mut UiDoubleBuffer {
        unsafe { &mut *self.shared_ptr }
    }

    pub fn get(&self) -> &UiDoubleBuffer {
        unsafe { &*self.shared_ptr }
    }
}

unsafe impl Send for SharedMemoryManager {}
unsafe impl Sync for SharedMemoryManager {}
