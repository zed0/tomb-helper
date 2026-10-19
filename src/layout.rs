//! Where things are in the game's structures, for the overlay and the `discover`/`check` tools. The defaults are
//! ROTTR (Steam)'s; a version's entry in the offsets file can override any of them in its "layout" section, e.g.
//! `"layout": {"instance": {"position": "0x20"}}`. Run `tomb-helper check` to test them against the running game.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::process_details::ProcessDetails;
use process_memory::Architecture;

use crate::offsets_file::Hex;

fn serialize_hex<S: Serializer>(value: &usize, serializer: S) -> Result<S::Ok, S::Error> {
    Hex(*value).serialize(serializer)
}

fn deserialize_hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    Hex::deserialize(deserializer).map(|h| h.0)
}

fn serialize_hex_option<S: Serializer>(value: &Option<usize>, serializer: S) -> Result<S::Ok, S::Error> {
    value.map(Hex).serialize(serializer)
}

fn deserialize_hex_option<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
    Option::<Hex>::deserialize(deserializer).map(|h| h.map(|h| h.0))
}

fn serialize_hex_list<S: Serializer>(values: &[usize], serializer: S) -> Result<S::Ok, S::Error> {
    values.iter().map(|v| Hex(*v)).collect::<Vec<_>>().serialize(serializer)
}

fn deserialize_hex_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<usize>, D::Error> {
    Vec::<Hex>::deserialize(deserializer).map(|l| l.into_iter().map(|h| h.0).collect())
}

// A group of hex numbers, each with its ROTTR (Steam) default
macro_rules! group {
    ($(#[$meta:meta])* $name:ident { $($(#[$field_meta:meta])* $field:ident = $default:expr,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $(
                $(#[$field_meta])*
                #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
                pub $field: usize,
            )*
        }

        impl Default for $name {
            fn default() -> Self {
                $name { $($field: $default,)* }
            }
        }
    };
}

/// Instances (game objects) are 0x120 byte records in a static array (`InstanceArray`); Lara is one of them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstanceLayout {
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub size: usize,
    /// u32, the same for every slot in the array (2), 0 after the end
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub header: usize,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub position: usize,
    /// Euler angles in radians, applied as Rx * Ry * Rz
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub rotation: usize,
    /// A pointer that's set while the slot is in use
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub in_use: usize,
    /// -> placement record, null for spawned instances. None (`null`) if the game has none (TR2013), when every instance
    /// goes by its object type's name.
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub placement: Option<usize>,
    /// u64 unique to the instance, which scripts refer to it by
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub id: usize,
    /// Pointers to follow from the instance to the name of its object type (e.g. "trx_mountain_hare"), which is all
    /// spawned instances have
    #[serde(serialize_with = "serialize_hex_list", deserialize_with = "deserialize_hex_list")]
    pub object_name: Vec<usize>,
    /// -> component table, with a slot per component type (found by their RTTI class names, see `components`). None if
    /// there's no table (TR2013, whose instances hold their components themselves)
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub components: Option<usize>,
    /// -> cdc::TransformComponent, null for unused slots. None if instances don't point at theirs (TR2013), when objects
    /// are drawn unscaled.
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub transform: Option<usize>,
    /// -> the data the instance's components were made from, which names them (see `component_data`). What kind of
    /// thing an instance is goes by these names in a game with no component table (TR2013).
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub component_data: Option<usize>,
}

impl Default for InstanceLayout {
    fn default() -> Self {
        InstanceLayout {
            size: 0x120,
            header: 0x0,
            position: 0x10,
            rotation: 0x30,
            in_use: 0xE0,
            placement: Some(0x78),
            id: 0x60,
            object_name: vec![0x80, 0x8, 0x58, 0x8],
            components: Some(0xD8),
            transform: Some(0xE0),
            component_data: None,
        }
    }
}

group! {
    /// What a placed instance was placed as
    PlacementLayout {
        /// -> e.g. "fx_fire_campfire_prelight1-neberle2"
        name = 0x48,
        /// -> e.g. "campsite_base_trx.fx_fire_campfire-neberle7"
        full_name = 0x50,
    }
}

group! {
    /// Every component
    ComponentLayout {
        /// -> its instance
        instance = 0x8,
    }
}

group! {
    /// An instance's component data (`instance.component_data`), only used if that's set. TR2013's are the defaults.
    ComponentDataLayout {
        /// u32
        count = 0x8,
        /// Entries: a hash, -> the component's name (e.g. "InteractComponent"), -> its data
        entries = 0x14,
        entry_size = 0xC,
        name = 0x4,
    }
}

group! {
    /// TR2013's aim sweep (see `aim_sweep`), from Lara through her inventory to the tuning of the weapon she has out. The
    /// defaults are TR2013 (Steam)'s, and it's only used if `InventoryPool` and `TypePool` are in the offsets file.
    AimSweepLayout {
        /// In Lara's proxy (`PlayerProxy`), -> her instance
        proxy_instance = 0x18,
        /// Handles (index, id) into pools of records: the pool starts with this many 0x10 byte entries, each the index of
        /// a record, with the id at +0x8, and the records follow
        max_handles = 0xBB8,
        handle_size = 0x10,
        handle_id = 0x8,
        records = 0xBC10,
        /// Instances by handle: index 0 is this far into the instance array, and each says which index and id it is here
        instance_base = 0xD0,
        instance_index = 0xC8,
        /// In an instance, the handle of its type (in `TypePool`), and for Lara, of her inventory (in `InventoryPool`)
        type_handle = 0x50,
        inventory_size = 0xC0,
        /// Which index and id the inventory record is
        inventory_index = 0xB8,
        /// The key of the item she has out, 0 if none
        current_item = 0x64,
        /// A map of items: -> buckets of node pointers, and how many
        map_buckets = 0x20,
        map_bucket_count = 0x2C,
        node_next = 0x0,
        node_key = 0x8,
        /// The handle of the item's instance
        node_item = 0x20,
        type_size = 0x250,
        type_index = 0x248,
        /// -> the type's data, in which the tuning block is at `block`
        type_data = 0x10,
        block = 0x298,
        /// In the tuning block, in Lara's space: where it's swept from, and to (less the radius)
        start = 0x0,
        middle = 0xC,
        radius = 0x60,
        /// From Lara's instance to her player object ([[instance + instance_player] + player], RTTI `Player`), and in
        /// that the movement it pushes her by this frame (the third of an array of movement by kind)
        instance_player = 0xA8,
        player = 0x14,
        push = 0x540,
    }
}

group! {
    /// cdc::TransformComponent
    TransformLayout {
        /// Row-major 4x4 object-to-world matrix. Not kept up to date for everything (Lara's stays where she loaded
        /// in), so only its scale is used.
        matrix = 0x30,
        /// -> live world space bone matrices, row-major. Bone n of a hit capsule is matrix n + 1.
        bones = 0x80,
        bone_size = 0x40,
    }
}

group! {
    /// The bounds of an instance's render model: cdc::DrawableComponent -> InstanceDrawable ->
    /// CommonRenderModelInstance -> CommonRenderModel -> Mesh resource
    DrawableLayout {
        /// Null for instances that aren't drawn (triggers, markers, ...)
        instance_drawable = 0x98,
        model_instance = 0xC8,
        model = 0x48,
        mesh = 0x50,
        /// In the Mesh header, which starts "Mesh"
        mesh_min = 0x20,
        mesh_max = 0x30,
    }
}

group! {
    /// Hit capsules ("DefensiveCapsule_head", ...) in a cdc::CollideComponent, each attached to a bone
    CollideLayout {
        /// -> entries
        shapes = 0x70,
        /// u32
        shape_count = 0x78,
        entry_size = 0x18,
        /// Byte 2 is the shape's type, the high u32 its bone
        entry_id = 0x8,
        /// -> shape in bone space
        entry_shape = 0x10,
        /// Capsules are this type (after `capsule_type_mask`), the others spheres. The top bit is set on Lara's and
        /// ROTTR's characters' (0x8A, spheres 0x81) but not on SOTTR's villagers' (0x0A).
        capsule_type = 0x0A,
        capsule_type_mask = 0x7F,
        capsule_centre = 0x0,
        /// Quaternion x, y, z, w; the capsule runs along its z axis
        capsule_rotation = 0x10,
        capsule_radius = 0x28,
        /// Of the cylinder part
        capsule_length = 0x2C,
        capsule_size = 0x30,
    }
}

/// Lara's movement collision, her cdc::ProxyComponent. It holds the shape in use, and the one it's changing to
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProxyLayout {
    /// Of the shape in use, in world space
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub centre: usize,
    /// Radius, then the length of the cylinder part
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub in_use_size: usize,
    /// Of the centre from Lara's feet, in her space
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub in_use_offset: usize,
    /// Tilt of the capsule (e.g. lying down when rolling), quaternion x, y, z, w
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub in_use_local_rotation: usize,
    /// Lara's facing
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub in_use_rotation: usize,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub next_size: usize,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub next_offset: usize,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub next_local_rotation: usize,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub next_rotation: usize,
    /// -> physics body the proxy is swept as, which the camera's and Lara's weapon's are attached to. None if there
    /// isn't one (TR2013)
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub body: Option<usize>,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub size: usize,
    /// What it hit this frame (including through the proxies attached to it): two u32 counts (next to each other in
    /// ROTTR, not in SOTTR; one in TR2013), and records with the world space point of contact (at their start, but
    /// after a normal in TR2013, so there this is where the first point is)
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub contact_count: usize,
    /// None if there's just the one (TR2013)
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub second_contact_count: Option<usize>,
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub contacts: Option<usize>,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub contact_size: usize,
    /// u32 flags, with `collides` set while an attached proxy (e.g. the combat proxy) collides, for a game where that isn't
    /// known from physics bodies (TR2013)
    #[serde(serialize_with = "serialize_hex_option", deserialize_with = "deserialize_hex_option")]
    pub flags: Option<usize>,
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub collides: usize,
}

impl Default for ProxyLayout {
    fn default() -> Self {
        ProxyLayout {
            centre: 0x30,
            in_use_size: 0x58,
            in_use_offset: 0x60,
            in_use_local_rotation: 0x80,
            in_use_rotation: 0x90,
            next_size: 0xC0,
            next_offset: 0xD0,
            next_local_rotation: 0xF0,
            next_rotation: 0x100,
            body: Some(0x148),
            size: 0x150,
            contact_count: 0x164,
            second_contact_count: Some(0x168),
            contacts: Some(0x190),
            contact_size: 0x60,
            flags: None,
            collides: 0x2,
        }
    }
}

group! {
    /// Physics bodies, which are in a list. The camera's and Lara's weapon's are attached to hers.
    BodyLayout {
        /// -> shape
        shape = 0x20,
        next = 0x30,
        parent = 0x48,
    }
}

group! {
    /// A physics body's shape
    PhysicsShapeLayout {
        /// World space
        centre = 0x0,
        /// World space quaternion x, y, z, w; capsules run along their z axis
        rotation = 0x20,
        /// Then the length of the cylinder part
        radius = 0x44,
        /// Byte
        shape_type = 0x98,
        capsule_type = 1,
        /// Byte, nonzero while it's touching something
        touching = 0x99,
        size = 0xA0,
    }
}

group! {
    /// The ground probes the game casts for Lara (`PlayerProbes`): a u32 count, then records
    ProbesLayout {
        records = 0x70,
        size = 0x60,
        /// Can be stale
        hit = 0x10,
        start = 0x20,
        end = 0x30,
    }
}

group! {
    /// GameRender (used by `discover` to write the camera's offsets, and for which floats of the lens are which)
    CameraLayout {
        /// World-to-view matrix (`CameraView`)
        view = 0x520,
        /// Near, far, near, far, fov, aspect, fov, aspect (`CameraLens`); TR2013 has just near, far, fov, aspect
        lens = 0x30,
        /// Which of the 8 floats read from `lens` are the near and far planes, the vertical fov and the aspect ratio
        lens_near = 2,
        lens_far = 3,
        lens_fov = 6,
        lens_aspect = 7,
        /// The first row of camera-to-world (`CameraSin`, `CameraCos`)
        sin = 0x70,
        cos = 0x74,
    }
}

group! {
    /// cdc::CollisionSceneClient (`CollisionScene`), which lists the level's collision mesh instances
    CollisionSceneLayout {
        /// -> first instance
        first = 0x28,
        /// u32
        count = 0x30,
    }
}

group! {
    /// A collision mesh instance
    CollisionInstanceLayout {
        /// How much to read
        size = 0x98,
        /// Row-major object-to-world matrix (row vectors)
        matrix = 0x0,
        /// World space bounds
        min = 0x40,
        max = 0x50,
        /// The matrix and bounds, which are reread each tick
        placement_size = 0x60,
        /// -> filter, shared by many
        filter = 0x60,
        mesh = 0x70,
        next = 0x88,
        /// u16, bit 0 swaps the first two corners of each triangle
        flags = 0x90,
        /// In the filter, a u32 of bits for which kinds of collision use the mesh
        filter_kinds = 0x4,
        /// The kind Lara's movement collides with
        player_kind = 0x2,
    }
}

group! {
    /// A collision mesh
    CollisionMeshLayout {
        header_size = 0x48,
        /// -> vertices
        vertices = 0x10,
        /// -> triangles: 3 u32 vertex indices, a flags byte and a material byte
        triangles = 0x18,
        /// u32
        triangle_count = 0x34,
        /// u32
        vertex_count = 0x38,
        /// u16
        vertex_format = 0x44,
        /// 3 f32s
        format_float = 1,
        /// 3 f16s
        format_half = 2,
        triangle_size = 0x10,
        triangle_flags = 0xC,
        /// The game's collision tests skip these
        triangle_ignored = 0x40,
    }
}

group! {
    /// A trigger's shape: cdc::ShapeComponent -> record, in the instance's space
    TriggerShapeLayout {
        /// In the component, -> record
        shape = 0x18,
        /// u32
        shape_type = 0x0,
        /// Floats, as many as the type needs
        size = 0x8,
        record_size = 0x38,
        /// Half extents x, y, z
        box_type = 1,
        /// An upright rectangle in the x/z plane, half width (x) and half height (z)
        plane_type = 3,
        /// Radius
        sphere_type = 4,
        /// A polygon swept along a vector for each corner ("triggerpolyvolume")
        poly_type = 5,
        /// u32
        poly_count = 0x8,
        /// -> entries: corner (x, y, z, 1), then how far it's swept
        poly_points = 0x10,
        poly_point_size = 0x20,
    }
}

group! {
    /// ActionGraph scripts: a graph (one per instance with a script) lists its nodes and the links between them. Found
    /// in SOTTR, the same in ROTTR
    ScriptLayout {
        /// In a node, -> its graph
        node_graph = 0x8,
        /// In a graph, -> its nodes (pointers, with gaps)
        graph_nodes = 0x78,
        /// In a graph, -> its links' header
        graph_links = 0x90,
        /// In the links' header, u32: how many nodes the links can refer to
        node_count = 0x0,
        /// u32
        link_count = 0x4,
        /// -> links: u16 from node, u16 to node (indices in the graph's nodes), u16 output, u16 input
        links = 0x10,
        link_size = 0x8,
        /// In an AGObjectNode, the id of the instance it acts on
        object_instance = 0x50,
        /// In an AGMessageSendNode, the id of the instance it sends a message to
        message_instance = 0x28,
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    pub instance: InstanceLayout,
    pub placement: PlacementLayout,
    pub component: ComponentLayout,
    pub component_data: ComponentDataLayout,
    pub transform: TransformLayout,
    pub drawable: DrawableLayout,
    pub collide: CollideLayout,
    pub proxy: ProxyLayout,
    pub body: BodyLayout,
    pub physics_shape: PhysicsShapeLayout,
    pub probes: ProbesLayout,
    pub aim_sweep: AimSweepLayout,
    pub camera: CameraLayout,
    pub collision_scene: CollisionSceneLayout,
    pub collision_instance: CollisionInstanceLayout,
    pub collision_mesh: CollisionMeshLayout,
    pub trigger_shape: TriggerShapeLayout,
    pub script: ScriptLayout,
}

impl Layout {
    pub fn is_default(&self) -> bool {
        *self == Layout::default()
    }
}

static CURRENT: RwLock<Option<Arc<Layout>>> = RwLock::new(None);
static DEFAULT: OnceLock<Arc<Layout>> = OnceLock::new();

/// The layout of the game tomb-helper is connected to (ROTTR (Steam)'s until it connects)
pub fn current() -> Arc<Layout> {
    if let Some(layout) = CURRENT.read().unwrap().as_ref() {
        return layout.clone();
    }
    DEFAULT.get_or_init(|| Arc::new(Layout::default())).clone()
}

pub fn set(layout: Layout) {
    *CURRENT.write().unwrap() = Some(Arc::new(layout));
}

static POINTER_SIZE: AtomicUsize = AtomicUsize::new(8);

/// Uses the layout of a game version, and its pointer size
pub fn set_for(details: &ProcessDetails) {
    set(details.layout.clone());
    let size = if matches!(details.arch, Architecture::Arch64Bit) { 8 } else { 4 };
    POINTER_SIZE.store(size, Ordering::Relaxed);
}

/// How big the game's pointers are: 8 bytes, or 4 for a 32-bit game (TR2013)
pub fn pointer_size() -> usize {
    POINTER_SIZE.load(Ordering::Relaxed)
}

/// The pointer at the start of `bytes`, at the game's pointer size
pub fn pointer_from(bytes: &[u8]) -> usize {
    let mut value = [0u8; 8];
    let size = pointer_size().min(bytes.len());
    value[..size].copy_from_slice(&bytes[..size]);
    u64::from_le_bytes(value) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_only_change_what_they_name() {
        let layout: Layout = serde_json::from_str(r#"{"instance": {"position": "0x20"}, "probes": {"end": 64}}"#).unwrap();
        assert_eq!(layout.instance.position, 0x20);
        assert_eq!(layout.instance.rotation, 0x30);
        assert_eq!(layout.probes.end, 0x40);
        assert_eq!(layout.collide, CollideLayout::default());
    }

    #[test]
    fn optional_offsets_can_be_turned_off() {
        let layout: Layout = serde_json::from_str(r#"{"instance": {"transform": null, "in_use": "0x40"}}"#).unwrap();
        assert_eq!(layout.instance.transform, None);
        assert_eq!(layout.instance.placement, Some(0x78));
        assert_eq!(layout.instance.in_use, 0x40);
        let json = serde_json::to_string(&layout).unwrap();
        assert!(json.contains(r#""transform":null"#), "{}", json);
    }

    #[test]
    fn rejects_typos() {
        assert!(serde_json::from_str::<Layout>(r#"{"instance": {"postion": "0x20"}}"#).is_err());
        assert!(serde_json::from_str::<Layout>(r#"{"instances": {}}"#).is_err());
    }

    #[test]
    fn round_trips_as_hex() {
        let json = serde_json::to_string(&Layout::default()).unwrap();
        assert!(json.contains(r#""position":"0x10""#), "{}", json);
        assert!(json.contains(r#""object_name":["0x80","0x8","0x58","0x8"]"#), "{}", json);
        assert_eq!(serde_json::from_str::<Layout>(&json).unwrap(), Layout::default());
    }
}
