use std::ptr;
use std::sync::atomic::{fence, AtomicUsize, Ordering};
use shared_memory::{Shmem, ShmemConf, ShmemError};
use crate::position::Position;

pub const MAX_OBJECTS: usize = 64;
pub const OBJECT_NAME_LENGTH: usize = 64;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ObjectBox {
    // Row-major object-to-world matrix as stored by the game (row vectors)
    pub transform: [f32; 16],
    // Box corners in object space
    pub min: Position,
    pub max: Position,
    // Nul padded placement name
    pub name: [u8; OBJECT_NAME_LENGTH],
}

impl ObjectBox {
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        std::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

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
}

#[repr(C)]
pub struct UiDoubleBuffer {
    active_buffer: AtomicUsize,
    // Odd while the matching buffer is being written
    sequences: [AtomicUsize; 2],
    buffers: [UiData; 2],
}

impl UiDoubleBuffer {
    pub fn set_next(&mut self, next_value: UiData) {
        let next = 1 - self.active_buffer.load(Ordering::Relaxed);
        self.sequences[next].fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        unsafe { ptr::write_volatile(&mut self.buffers[next], next_value) };
        self.sequences[next].fetch_add(1, Ordering::Release);
        self.active_buffer.store(next, Ordering::Release);
    }

    // The writer is in another process and doesn't wait for readers, so it can start rewriting a buffer
    // while it's being copied. Returns None if that happened.
    pub fn try_current(&self) -> Option<UiData> {
        let current = self.active_buffer.load(Ordering::Acquire);
        let before = self.sequences[current].load(Ordering::Acquire);
        if before % 2 == 1 {
            return None;
        }
        let value = unsafe { ptr::read_volatile(&self.buffers[current]) };
        fence(Ordering::Acquire);
        if self.sequences[current].load(Ordering::Relaxed) != before {
            return None;
        }
        Some(value)
    }

    pub fn current(&self) -> UiData {
        loop {
            if let Some(value) = self.try_current() {
                return value;
            }
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
