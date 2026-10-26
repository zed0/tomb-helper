//! What a game version is (`ProcessDetails`, `VersionIdentifier`) and the values tomb-helper reads from it
//! (`AddressType`, whose serde names are the offsets file's keys). Types only; the versions are in `offsets.json`.

use crate::layout::Layout;
use process_memory::Architecture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// The offsets for each known version are in `offsets.json` (see `offsets_file`)

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AddressType {
    Version,
    XPosition,
    YPosition,
    ZPosition,
    XLookAt,
    YLookAt,
    ZLookAt,
    // TODO: Work out a more sensible way of representing the camera matrix
    // Note: The easiest way to find these values is to search for camera XYZ values which form the
    // last entries of the extrinsic camera matrix (https://ksimek.github.io/2012/08/22/extrinsic/)
    // From there the first 2 entries of the matrix can be used as CameraSin and CameraCos
    CameraSin,
    CameraCos,
    // Row-major 4x4 world-to-view matrix (row vectors, as used by the game)
    CameraView,
    // 8 floats with the near and far planes, vertical fov (radians) and aspect ratio somewhere in them
    // (`layout.camera.lens_*`; ROTTR's are near, far, near, far, fov, aspect, fov, aspect)
    CameraLens,
    // Static array of instance (game object) records (`layout.instance.size` bytes each), see `object_list`
    InstanceArray,
    // Count and records of the ground probes the game casts for Lara, see `collision::read_probes`
    PlayerProbes,
    // Lara's movement proxy (see `collision::read_proxy`), for a game where it isn't found through her components:
    // TR2013 keeps it in a static
    PlayerProxy,
    // The combat proxy, attached to Lara's, which collides while she aims, where it isn't found through her proxy's
    // physics body. That's how it's found in TR2013 Steam, so only v1.01.748 still has this, until its body offset
    // (+0x10C, from its code) is checked live; then it can go
    CombatProxy,
    // TR2013: pools of inventory and type records (see `aim_sweep`), to find the tuning of the weapon Lara has out
    InventoryPool,
    TypePool,
    // The cdc::CollisionSceneClient listing the level's collision meshes (resolves to its address), see
    // `collision_mesh`
    CollisionScene,
    // Lara's velocity (3 floats, her cdc::BallisticComponent's), which flight mode zeroes, or she's dragged down a
    // little each frame by the speed she builds up falling
    Velocity,
    // The height she was last on the ground at (two copies), which fall damage goes by; flight mode moves it with her,
    // or she dies when flown down too far below it
    FallStart,
    FallStartCopy,
    // Lara's proxy's centre (3 floats, which follows her), and the centre it's been moved to by collision, which the
    // game moves her towards (so back through a wall she's flown through); flight mode puts the second where the first is
    ProxyCentre,
    ProxyResolvedCentre,
    // Lara's proxy's u32 flags, with `layout.proxy.movement_collides` (0x1) set while it collides, which flight mode
    // clears, or it's pushed back up out of the ground and she's pulled up after it
    ProxyFlags,
    // Lara's proxy's physics body: its shape's centre and a copy (3 floats), and how far it has left to go to get to her,
    // in the body and its shape (4 floats). With its proxy's collision off, the body stays where it was, and after, it
    // only moves towards her, so flight mode puts it back on her afterwards
    ProxyShapeCentre,
    ProxyShapePreviousCentre,
    ProxyShapeGap,
    ProxyBodyGap,
    // A byte, nonzero while the camera collides with the level (the active camera controller's); flight mode turns it
    // off, or the camera stays on this side of a wall she's flown through
    CameraCollision,
    CutscenePrompt,
    CutsceneStatus,
    CutsceneTimeline,
    CutsceneLength,
    CutsceneId,
}

pub type AddressOffsets = HashMap<AddressType, Vec<usize>>;

#[derive(Debug, Clone)]
pub struct VersionIdentifier {
    // Version string
    pub version: String,
    // Size of the image in memory. Used to differentiate between Steam and Epic Games versions since they share the same version string
    // https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#optional-header-windows-specific-fields-image-only
    pub image_size: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ProcessDetails {
    pub name: String,
    pub executable_name: String,
    pub version: VersionIdentifier,
    pub arch: Architecture,
    pub address_offsets: AddressOffsets,
    // RVAs of instructions that access each value, found with `tomb-helper watch`. Only used to
    // derive offsets for new versions.
    pub witnesses: HashMap<AddressType, Vec<usize>>,
    // The same for layout fields (`group.field`), found with `tomb-helper watch <group.field>`
    pub layout_witnesses: HashMap<String, Vec<usize>>,
    // Offsets a tool found but couldn't confirm, kept in the file for review but not used
    pub unverified: HashMap<AddressType, Vec<usize>>,
    // Where things are in the game's structures, see `layout`
    pub layout: Layout,
}
