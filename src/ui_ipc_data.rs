use std::sync::atomic::{AtomicUsize, Ordering};
use shared_memory::{Shmem, ShmemConf, ShmemError};
use crate::position::Position;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Trigger {
    pub origin: Position,
    pub size: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct UiData {
    pub debug: bool,
    pub should_exit: bool,
    pub position: Position,
    pub triggers: [Option<Trigger>; 10],
    pub camera_extrinsics: [f32; 16],
    pub camera_intrinsics: [[f32; 16]; 100],
}

#[repr(C)]
pub struct UiDoubleBuffer {
    active_buffer: AtomicUsize,
    buffers: [UiData; 2],
}

impl UiDoubleBuffer {
    pub fn set_next(&mut self, next_value: UiData) {
        let current = self.active_buffer.load(Ordering::Relaxed);
        let next = 1 - current;
        self.buffers[next] = next_value;
        self.active_buffer.store(next, Ordering::Release);
    }

    pub fn current(&self) -> UiData {
        let current = self.active_buffer.load(Ordering::Acquire);
        self.buffers[current]
    }
}

pub struct SharedMemoryManager {
    shmem: Shmem,
    shared_ptr: *mut UiDoubleBuffer,
}

impl SharedMemoryManager {
    pub fn new(is_owner: bool) -> Self {
        let mut shmem = match ShmemConf::new().size(size_of::<UiDoubleBuffer>()).os_id("TombHelperIPC").create() {
            Ok(m) => m,
            Err(ShmemError::MappingIdExists) => ShmemConf::new().os_id("TombHelperIPC").open().unwrap(),
            Err(e) => panic!("Unable to open IPC! {}", e),
        };
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
