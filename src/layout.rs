//! Where things are in the game's structures, for the overlay and the `discover`/`check` tools. The defaults are
//! ROTTR (Steam)'s; a version's entry in the offsets file can override any of them in its "layout" section, e.g.
//! `"layout": {"instance": {"position": "0x20"}}`. Run `tomb-helper check` to test them against the running game.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Arc, OnceLock, RwLock};

use crate::offsets_file::Hex;

fn serialize_hex<S: Serializer>(value: &usize, serializer: S) -> Result<S::Ok, S::Error> {
    Hex(*value).serialize(serializer)
}

fn deserialize_hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    Hex::deserialize(deserializer).map(|h| h.0)
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
    /// -> placement record, null for spawned instances
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub placement: usize,
    /// u64 unique to the instance, which scripts refer to it by
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub id: usize,
    /// Pointers to follow from the instance to the name of its object type (e.g. "trx_mountain_hare"), which is all
    /// spawned instances have
    #[serde(serialize_with = "serialize_hex_list", deserialize_with = "deserialize_hex_list")]
    pub object_name: Vec<usize>,
    /// -> component table, with a slot per component type (found by their RTTI class names, see `components`)
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub components: usize,
    /// -> cdc::TransformComponent, null for unused slots
    #[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")]
    pub transform: usize,
}

impl Default for InstanceLayout {
    fn default() -> Self {
        InstanceLayout {
            size: 0x120,
            header: 0x0,
            position: 0x10,
            rotation: 0x30,
            placement: 0x78,
            id: 0x60,
            object_name: vec![0x80, 0x8, 0x58, 0x8],
            components: 0xD8,
            transform: 0xE0,
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

group! {
    /// Lara's movement collision, her cdc::ProxyComponent. It holds the shape in use, and the one it's changing to
    /// (e.g. when she starts aiming), which the shape in use eases towards over a few frames.
    ProxyLayout {
        /// Of the shape in use, in world space
        centre = 0x30,
        /// Radius, then the length of the cylinder part
        in_use_size = 0x58,
        /// Of the centre from Lara's feet, in her space
        in_use_offset = 0x60,
        /// Tilt of the capsule (e.g. lying down when rolling), quaternion x, y, z, w
        in_use_local_rotation = 0x80,
        /// Lara's facing
        in_use_rotation = 0x90,
        next_size = 0xC0,
        next_offset = 0xD0,
        next_local_rotation = 0xF0,
        next_rotation = 0x100,
        /// -> physics body the proxy is swept as
        body = 0x148,
        size = 0x150,
        /// What it hit this frame (including through the proxies attached to it): two u32 counts (next to each other
        /// in ROTTR, not in SOTTR), and records starting with the world space point of contact
        contact_count = 0x164,
        second_contact_count = 0x168,
        contacts = 0x190,
        contact_size = 0x60,
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
    /// GameRender (only used by `discover` to write the camera's offsets)
    CameraLayout {
        /// World-to-view matrix (`CameraView`)
        view = 0x520,
        /// Near, far, near, far, fov, aspect, fov, aspect (`CameraLens`)
        lens = 0x30,
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
    /// in SOTTR, unverified in ROTTR
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
    pub transform: TransformLayout,
    pub drawable: DrawableLayout,
    pub collide: CollideLayout,
    pub proxy: ProxyLayout,
    pub body: BodyLayout,
    pub physics_shape: PhysicsShapeLayout,
    pub probes: ProbesLayout,
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
