use std::ptr;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicUsize, Ordering};
use shared_memory::{Shmem, ShmemConf, ShmemError};
use crate::position::Position;

pub const MAX_OBJECTS: usize = 256;
pub const MAX_CAPSULES: usize = 32;
pub const MAX_PROBES: usize = 8;
pub const MAX_PROXIES: usize = 2;
pub const MAX_ATTACHED: usize = 8;
pub const OBJECT_NAME_LENGTH: usize = 64;
// Its name, what sets it off and what it does
pub const TRIGGER_LABEL_LENGTH: usize = 128;
pub const MAX_TRIANGLES: usize = 163_840;
pub const MAX_TRIGGERS: usize = 256;
pub const MAX_TRIGGER_LINES: usize = 16384;

#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoxKind {
    // The object has no model, so the box is just `object_box_size`
    Estimated,
    // The bounds of the object's model
    Model,
    Lara,
}

// Why an object is shown, which sets its colour
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ObjectCategory {
    // Matched the configured name filters
    Named,
    // Something Lara can interact with (campfires, pickups, documents, ...)
    Interactable,
    // Characters and animals
    Npc,
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
    pub category: ObjectCategory,
    // Nul padded placement name
    pub name: [u8; OBJECT_NAME_LENGTH],
}

impl ObjectBox {
    pub fn name(&self) -> &str {
        name_from(&self.name)
    }
}

// What a trigger is for, going by its name (see `trigger::categorise`)
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerCategory {
    Checkpoint,
    Death,
    Streaming,
    Combat,
    // Anything whose name doesn't say, e.g. the level scripts' unnamed `triggerplane`s
    Unknown,
    Camera,
    Effects,
    Hints,
    // Attached to bushes, collectibles, throwables, campfires, ...
    Objects,
}

impl TriggerCategory {
    pub const ALL: [TriggerCategory; 9] = [
        TriggerCategory::Checkpoint,
        TriggerCategory::Death,
        TriggerCategory::Streaming,
        TriggerCategory::Combat,
        TriggerCategory::Unknown,
        TriggerCategory::Camera,
        TriggerCategory::Effects,
        TriggerCategory::Hints,
        TriggerCategory::Objects,
    ];

    // As used in the config
    pub fn name(self) -> &'static str {
        match self {
            TriggerCategory::Checkpoint => "checkpoints",
            TriggerCategory::Death => "death",
            TriggerCategory::Streaming => "streaming",
            TriggerCategory::Combat => "combat",
            TriggerCategory::Unknown => "unknown",
            TriggerCategory::Camera => "camera",
            TriggerCategory::Effects => "effects",
            TriggerCategory::Hints => "hints",
            TriggerCategory::Objects => "objects",
        }
    }

    pub fn bit(self) -> u32 {
        1 << self as u32
    }

    // The bits for the named categories
    pub fn mask(names: &[String]) -> u32 {
        TriggerCategory::ALL
            .iter()
            .filter(|c| names.iter().any(|n| n.eq_ignore_ascii_case(c.name())))
            .map(|c| c.bit())
            .sum()
    }
}

// What triggers can be shown by: what their scripts lead to (see `trigger_scripts`), then whether they act on a
// particular instance, and the ones that don't do anything known. Each is a bit, in this order. The names are used in
// the labels and the config.
pub const TRIGGER_ACTIONS: [&str; 28] = [
    "checkpoint",
    "objective",
    "progress",
    "cutscene",
    "moveset",
    "camera",
    "streaming",
    "teleport",
    "combat",
    "spawn",
    "despawn",
    "damage",
    "dialogue",
    "tutorial",
    "look at",
    "NPC",
    "inventory",
    "campsite",
    "lighting",
    "weather",
    "audio",
    "UI",
    "rumble",
    "saves state",
    // Acts on or sends messages to a particular instance
    "instances",
    // Has a script, but nothing it does is known
    "something else",
    "no script",
    // Not looked for yet
    "unchecked",
];

pub fn trigger_action_bit(name: &str) -> u32 {
    TRIGGER_ACTIONS.iter().position(|a| a.eq_ignore_ascii_case(name)).map_or(0, |i| 1 << i)
}

pub fn trigger_action_mask(names: &[String]) -> u32 {
    names.iter().map(|n| trigger_action_bit(n)).fold(0, |mask, bit| mask | bit)
}

// A trigger's name and where to put it, its outline is in `UiData::trigger_lines`
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct TriggerLabel {
    pub position: Position,
    pub category: TriggerCategory,
    pub name: [u8; TRIGGER_LABEL_LENGTH],
}

impl TriggerLabel {
    pub fn name(&self) -> &str {
        name_from(&self.name)
    }
}

// Part of a trigger's outline, in world space
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Line {
    pub a: Position,
    pub b: Position,
    pub category: TriggerCategory,
}

// Nul padded
pub fn name_bytes(name: &str) -> [u8; OBJECT_NAME_LENGTH] {
    text_bytes(name)
}

pub fn text_bytes<const N: usize>(text: &str) -> [u8; N] {
    let mut bytes = [0u8; N];
    let length = text.len().min(N - 1);
    bytes[..length].copy_from_slice(&text.as_bytes()[..length]);
    bytes
}

fn name_from(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    // A name cut short can end part way through a character
    match std::str::from_utf8(&bytes[..end]) {
        Ok(name) => name,
        Err(e) => std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or("?"),
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
    // From a mesh Lara doesn't collide with (e.g. every log of a pile, where she collides with a rough box round them)
    pub detailed: bool,
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
    // Where `CameraView` and `CameraLens` are in the game, so the overlay can read the camera for the frame it's
    // drawing on. The ones above lag behind, which makes everything judder as the camera turns.
    pub camera_view_address: usize,
    pub camera_lens_address: usize,
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
    // Trigger volumes near Lara
    pub trigger_count: usize,
    pub triggers: [TriggerLabel; MAX_TRIGGERS],
    pub trigger_line_count: usize,
    pub trigger_lines: [Line; MAX_TRIGGER_LINES],
    // The level's collision near Lara, closest first. Last, as only the used part of it is copied.
    pub triangle_count: usize,
    // How long the exe took to find them
    pub triangle_milliseconds: f32,
    pub triangles: [Triangle; MAX_TRIANGLES],
}

impl UiData {
    // Bytes up to the end of the triangles in use, which is all that needs copying. The count is read
    // volatile and clamped as it may be mid write.
    unsafe fn used_size(data: *const UiData) -> usize {
        let count = ptr::read_volatile(ptr::addr_of!((*data).triangle_count)).min(MAX_TRIANGLES);
        std::mem::offset_of!(UiData, triangles) + count * size_of::<Triangle>()
    }

    unsafe fn copy_used(from: *const UiData, to: *mut UiData) {
        ptr::copy_nonoverlapping(from as *const u8, to as *mut u8, UiData::used_size(from));
    }

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

// The game's projection matrix is reused for another pass with a narrower fov part way through each
// frame, so build the main camera's from its lens instead: near, far, near, far, fov, aspect, fov, aspect.
// The second copies are the more stable ones.
pub fn projection_from_lens(lens: &[f32; 8]) -> [f32; 16] {
    let (near, far, fov, aspect) = (lens[2], lens[3], lens[6], lens[7]);
    let y = 1.0 / (fov / 2.0).tan();
    let z = far / (far - near);
    // Row-major for row vectors, like the game's
    [
        y / aspect, 0.0, 0.0, 0.0,
        0.0, y, 0.0, 0.0,
        0.0, 0.0, z, 1.0,
        0.0, 0.0, -near * z, 0.0,
    ]
}

// What to show, which the overlay's window and the exe's hotkeys change. Kept apart from the buffers since the exe
// overwrites those.
#[repr(C)]
pub struct UiSettings {
    // Whether the exe has set the starting values from its config
    pub initialised: AtomicBool,
    // Everything to do with collision (Lara's and the meshes), whatever the settings below
    pub collision: AtomicBool,
    // Lara's capsules and probes
    pub lara_collision: AtomicBool,
    // The level's collision meshes Lara collides with, and the rest
    pub coarse_meshes: AtomicBool,
    pub detailed_meshes: AtomicBool,
    // How far from Lara to draw each of them (f32 bits)
    pub coarse_radius: AtomicU32,
    pub detailed_radius: AtomicU32,
    // Scales how opaque the meshes and Lara's collision are drawn (f32 bits, 0 to 1)
    pub mesh_opacity: AtomicU32,
    // Draw the meshes fainter further from Lara, by (1 - distance / radius) to the power of the strength (f32 bits)
    pub fade_with_distance: AtomicBool,
    pub fade_strength: AtomicU32,
    // Only draw the triangles not hidden behind others
    pub cull_hidden: AtomicBool,
    // Boxes round things Lara can interact with, and trigger volumes
    pub interactables: AtomicBool,
    pub triggers: AtomicBool,
    pub npcs: AtomicBool,
    // Show triggers by `TRIGGER_ACTIONS` rather than `TriggerCategory`s, and which of each to show, as bits
    pub triggers_by_action: AtomicBool,
    pub trigger_categories: AtomicU32,
    pub trigger_actions: AtomicU32,
}

impl UiSettings {
    pub fn get(setting: &AtomicBool) -> bool {
        setting.load(Ordering::Relaxed)
    }

    pub fn set(setting: &AtomicBool, value: bool) {
        setting.store(value, Ordering::Relaxed);
    }

    pub fn toggle(setting: &AtomicBool) {
        setting.fetch_xor(true, Ordering::Relaxed);
    }

    pub fn get_u32(setting: &AtomicU32) -> u32 {
        setting.load(Ordering::Relaxed)
    }

    pub fn set_u32(setting: &AtomicU32, value: u32) {
        setting.store(value, Ordering::Relaxed);
    }

    pub fn get_f32(setting: &AtomicU32) -> f32 {
        f32::from_bits(setting.load(Ordering::Relaxed))
    }

    pub fn set_f32(setting: &AtomicU32, value: f32) {
        setting.store(value.to_bits(), Ordering::Relaxed);
    }
}

#[repr(C)]
pub struct UiDoubleBuffer {
    pub settings: UiSettings,
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
            UiData::copy_used(buffers.add(current), buffers.add(next));
            change(&mut *buffers.add(next));
        }
        self.sequences[next].fetch_add(1, Ordering::Release);
        self.active_buffer.store(next, Ordering::Release);
    }

    // The writer is in another process and doesn't wait for readers, so it can start rewriting a buffer
    // while it's being copied. Returns false if that happened, in which case `out` is left part written. Triangles
    // past `triangle_count` aren't copied.
    pub fn try_read(&self, out: &mut UiData) -> bool {
        let current = self.active_buffer.load(Ordering::Acquire);
        let before = self.sequences[current].load(Ordering::Acquire);
        if before % 2 == 1 {
            return false;
        }
        unsafe { UiData::copy_used(&self.buffers[current], out) };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_triangles_in_use_are_copied() {
        let corner = Position { x: 1.0, y: 2.0, z: 3.0 };
        let triangle = Triangle { corners: [corner; 3], kind: SurfaceKind::Wall, detailed: true };
        let mut from = UiData::zeroed();
        from.triangle_count = 2;
        from.triangles[..3].copy_from_slice(&[triangle; 3]);
        from.position = corner;
        let mut to = UiData::zeroed();
        unsafe { UiData::copy_used(&*from, &mut *to) };
        assert_eq!(to.triangle_count, 2);
        assert_eq!(to.position.z, 3.0);
        assert!(to.triangles[1].detailed);
        assert!(!to.triangles[2].detailed);
    }
}
