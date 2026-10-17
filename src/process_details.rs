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
    // 8 floats: near, far, near, far, vertical fov (radians), aspect, fov, aspect
    CameraLens,
    // Static array of 0x120 byte instance (game object) records, see `object_list`
    InstanceArray,
    // Lara's instance record (resolves to its address, not a value)
    PlayerInstance,
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
}
