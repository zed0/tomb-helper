//! What the exe sends the overlay, in shared memory (`shared_memory`), which both build: `UiData` (the camera, the keys
//! bound to the window's controls (`HotkeyLabel`), object boxes, triggers, capsules, probes and collision triangles) and
//! `UiSettings` (the overlay window's settings, which its checkboxes and the exe's hotkeys change). `UiData` is ~7MB, so it's double buffered and changed and read in
//! place, with a sequence count per buffer (`UiDoubleBuffer`): the exe never waits, and the overlay keeps its last good
//! copy when a read catches a write. The mapping is named after `UiDoubleBuffer`'s size, so one left from another
//! layout is never opened. Nothing in it may be pointer sized, as the 32-bit overlay reads it too.

use std::ptr;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};
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
// Actions the overlay's window can ask for (`UiSettings::requests`)
pub const REQUEST_STORE_POSITION: u32 = 1;
pub const REQUEST_RESTORE_POSITION: u32 = 2;
pub const REQUEST_TOGGLE_FLYING: u32 = 4;
pub const MAX_TRIGGER_LINES: usize = 16384;
pub const MAX_TRIGGER_FACES: usize = 32768;
// The keys bound to a control, e.g. "F5, NumPad5"
pub const HOTKEY_LABEL_LENGTH: usize = 32;

/// The overlay window's controls a hotkey can be bound to, which show its key (`UiData::hotkeys`, by this index)
#[repr(u32)]
#[derive(Copy, Clone, Debug)]
pub enum HotkeyLabel {
    Flight,
    SavePosition,
    RestorePosition,
    Window,
    Overlay,
    Collision,
    LaraCollision,
    CoarseMeshes,
    DetailedMeshes,
    Interactables,
    Triggers,
    Npcs,
}

pub const HOTKEY_LABELS: usize = 12;

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
    pub action: u32,
    pub name: [u8; TRIGGER_LABEL_LENGTH],
}

impl TriggerLabel {
    pub fn name(&self) -> &str {
        name_from(&self.name)
    }
}

// Part of a trigger's surface, in world space (drawn from both sides)
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct TriggerFace {
    pub corners: [Position; 3],
    pub category: TriggerCategory,
    // The `TRIGGER_ACTIONS` index it's coloured by when shown by what triggers do
    pub action: u32,
}

// Part of a trigger's outline, in world space
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Line {
    pub a: Position,
    pub b: Position,
    pub category: TriggerCategory,
    pub action: u32,
    // Lara's in the trigger
    pub inside: bool,
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

/// The keys bound to one of the window's controls, e.g. "F5", or "" if none
pub fn hotkey(data: &UiData, control: HotkeyLabel) -> &str {
    name_from(&data.hotkeys[control as usize])
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
// Shared between the 64-bit exe and the overlay, which is 32-bit in a 32-bit game, so nothing in it is pointer sized
pub struct UiData {
    pub debug: bool,
    pub should_exit: bool,
    pub position: Position,
    // The position tools (`PositionHandler`), if this game has them
    pub position_tools: bool,
    pub has_saved_position: bool,
    pub saved_position: Position,
    pub flying: bool,
    // The keys bound to each of the window's controls (`HotkeyLabel`), nul padded, empty if none
    pub hotkeys: [[u8; HOTKEY_LABEL_LENGTH]; HOTKEY_LABELS],
    // Where the collision meshes are drawn round (Lara, or where she's aiming), and whether she's aiming at it
    pub mesh_centre: Position,
    pub aiming: bool,
    // Row-major world-to-view and projection matrices as stored by the game (row vectors)
    pub camera_view: [f32; 16],
    pub camera_projection: [f32; 16],
    // Where `CameraView` and `CameraLens` are in the game, so the overlay can read the camera for the frame it's
    // drawing on. The ones above lag behind, which makes everything judder as the camera turns. 0 if the game has no
    // camera offsets, when nothing is drawn in the world.
    pub camera_view_address: u64,
    pub camera_lens_address: u64,
    // Which of the lens's floats are its near, far, fov and aspect (`projection_from_lens`)
    pub lens_fields: [u32; 4],
    pub object_count: u32,
    pub objects: [ObjectBox; MAX_OBJECTS],
    // Lara's collision
    pub capsule_count: u32,
    pub capsules: [Capsule; MAX_CAPSULES],
    pub probe_count: u32,
    pub probes: [Probe; MAX_PROBES],
    // Lara's movement collision: her proxy's shape, and the one it's changing to if that's different
    pub proxy_count: u32,
    pub proxies: [Capsule; MAX_PROXIES],
    // Whether the shape in use is pushing her off something
    pub proxy_touching: bool,
    // Proxies attached to Lara's, e.g. the one that moves behind her when she aims
    pub attached_count: u32,
    pub attached: [Capsule; MAX_ATTACHED],
    pub attached_touching: [bool; MAX_ATTACHED],
    // They only collide while Lara aims
    pub attached_active: bool,
    // Trigger volumes near Lara
    pub trigger_count: u32,
    pub triggers: [TriggerLabel; MAX_TRIGGERS],
    pub trigger_line_count: u32,
    pub trigger_lines: [Line; MAX_TRIGGER_LINES],
    pub trigger_face_count: u32,
    pub trigger_faces: [TriggerFace; MAX_TRIGGER_FACES],
    // The level's collision near Lara, closest first. Last, as only the used part of it is copied.
    pub triangle_count: u32,
    // How long the exe took to find them
    pub triangle_milliseconds: f32,
    // How many of the level's collision instances have each kind bit
    pub mesh_kind_counts: [u32; 16],
    pub triangles: [Triangle; MAX_TRIANGLES],
}

impl UiData {
    // Bytes up to the end of the triangles in use, which is all that needs copying. The count is read
    // volatile and clamped as it may be mid write.
    unsafe fn used_size(data: *const UiData) -> usize {
        let count = (ptr::read_volatile(ptr::addr_of!((*data).triangle_count)) as usize).min(MAX_TRIANGLES);
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
pub fn projection_from_lens(lens: &[f32; 8], fields: [u32; 4]) -> [f32; 16] {
    let field = |i: usize| lens[(fields[i] as usize).min(lens.len() - 1)];
    let (near, far, fov, aspect) = (field(0), field(1), field(2), field(3));
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
    pub initialised: BoolSetting,
    // The settings window (what's drawn in the world stays either way)
    pub show_window: BoolSetting,
    // `REQUEST_*` bits for actions the window's buttons ask for, which the exe takes
    pub requests: AtomicU32,
    // Everything drawn in the world (`ToggleOverlay`), whatever the settings below; the window stays
    pub overlay: BoolSetting,
    // Everything to do with collision (Lara's and the meshes), whatever the settings below
    pub collision: BoolSetting,
    // Lara's capsules and probes
    pub lara_collision: BoolSetting,
    // The level's collision meshes Lara collides with, and the rest
    pub coarse_meshes: BoolSetting,
    pub detailed_meshes: BoolSetting,
    // Which kind bits the other ("detailed") collision is drawn for
    pub other_kinds: U32Setting,
    // How far from Lara to draw each of them
    pub coarse_radius: F32Setting,
    pub detailed_radius: F32Setting,
    // Scales how opaque the meshes and Lara's collision are drawn (0 to 1)
    pub mesh_opacity: F32Setting,
    // Draw the meshes fainter further from Lara, by (1 - distance / radius) to the power of the strength
    pub fade_with_distance: BoolSetting,
    pub fade_strength: F32Setting,
    // Only draw the triangles not hidden behind others
    pub cull_hidden: BoolSetting,
    // Draw the collision meshes round where Lara's aiming while she aims
    pub follow_aim: BoolSetting,
    // How opaque trigger volumes are filled, 0 to 1
    pub trigger_opacity: F32Setting,
    // Boxes round things Lara can interact with, and trigger volumes
    pub interactables: BoolSetting,
    pub triggers: BoolSetting,
    pub npcs: BoolSetting,
    // Show triggers by `TRIGGER_ACTIONS` rather than `TriggerCategory`s, and which of each to show, as bits
    pub triggers_by_action: BoolSetting,
    pub trigger_categories: U32Setting,
    pub trigger_actions: U32Setting,
}

// Settings either side can change at any time, so they're atomics (in shared memory, so the same size as their values)
#[repr(transparent)]
pub struct BoolSetting(AtomicBool);

impl BoolSetting {
    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, value: bool) {
        self.0.store(value, Ordering::Relaxed);
    }

    pub fn toggle(&self) {
        self.0.fetch_xor(true, Ordering::Relaxed);
    }
}

#[repr(transparent)]
pub struct U32Setting(AtomicU32);

impl U32Setting {
    pub fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, value: u32) {
        self.0.store(value, Ordering::Relaxed);
    }
}

#[repr(transparent)]
pub struct F32Setting(AtomicU32);

impl F32Setting {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

#[repr(C)]
pub struct UiDoubleBuffer {
    pub settings: UiSettings,
    active_buffer: AtomicU32,
    // Odd while the matching buffer is being written
    sequences: [AtomicU32; 2],
    buffers: [UiData; 2],
}

impl UiDoubleBuffer {
    // Copies the current value into the other buffer, changes it there, then makes that one current. Done in
    // place since `UiData` is too big to pass around by value.
    pub fn update(&mut self, change: impl FnOnce(&mut UiData)) {
        let current = self.active_buffer.load(Ordering::Relaxed) as usize;
        let next = 1 - current;
        self.sequences[next].fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        let buffers = self.buffers.as_mut_ptr();
        unsafe {
            UiData::copy_used(buffers.add(current), buffers.add(next));
            change(&mut *buffers.add(next));
        }
        self.sequences[next].fetch_add(1, Ordering::Release);
        self.active_buffer.store(next as u32, Ordering::Release);
    }

    // Changes with every update, so the reader can tell the writer is still there
    pub fn generation(&self) -> u32 {
        self.sequences[0].load(Ordering::Relaxed).wrapping_add(self.sequences[1].load(Ordering::Relaxed))
    }

    // The writer is in another process and doesn't wait for readers, so it can start rewriting a buffer
    // while it's being copied. Returns false if that happened, in which case `out` is left part written. Triangles
    // past `triangle_count` aren't copied.
    pub fn try_read(&self, out: &mut UiData) -> bool {
        let current = self.active_buffer.load(Ordering::Acquire) as usize;
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
