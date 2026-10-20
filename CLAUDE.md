# tomb-helper — project index

Speedrun helper for the Tomb Raider reboot trilogy (TR2013, ROTTR, SOTTR). Windows-only Rust tool that
attaches to the running game, reads/writes its memory via pointer chains, and reacts to global hotkeys.
User docs (hotkeys, config, cutscene timing generation) live in `README.md`.

## Build outputs (two crates from one package — see `Cargo.toml`)

| Target | Entry | Output | Purpose |
|---|---|---|---|
| bin `tomb-helper` | `src/main.rs` | `tomb-helper.exe` | Main external process: finds game, runs handlers, listens to hotkeys |
| lib `tomb_helper_ui` (cdylib) | `src/lib.rs` | `tomb_helper_ui.dll` | ImGui overlay injected *into* the game (DX12 via `hudhook`; built for `i686-pc-windows-msvc` it's DX11 for TR2013, copied next to the exe as `tomb_helper_ui32.dll`) |

- The bin and lib share `src/ui_ipc_data.rs` and `src/position.rs` by each declaring them with `mod` —
  they're compiled into both, so `UiData` must stay `#[repr(C)]` with identical layout on both sides, including in the
  32-bit overlay for TR2013: nothing in the shared memory may be pointer sized (`u64` addresses, `u32` counts).
- `hudhook` (0.9.x) is patched (`[patch.'crates-io']`) to the `get-process-by-pid` branch of
  github.com/zed0/hudhook, which is upstream `main` plus `Process::by_pid()` in `src/inject.rs` (not upstream; used by
  `OverlayHandler`); `Cargo.lock` pins its commit (`cargo update -p hudhook` picks up new ones). A local checkout
  at `H:\hudhook` tracks the same branch; to build against local changes there, add
  `--config "patch.crates-io.hudhook.path='H:/hudhook'"`. The patch's version must match the
  `hudhook = "..."` requirement or cargo silently ignores it and uses crates.io.
- CI: `.github/workflows/rust.yml` (build + test on push/PR to master), `main.yml` (release on `v*` tag,
  uploads only `tomb-helper.exe`). Versioning via `cargo-release`.
- `.cargo/config.toml` holds the `crt-static` rustflags (static CRT so the exe/dll run standalone).
  It must stay under `.cargo/` — cargo ignores a `config.toml` at the repo root.

## Runtime flow (`src/main.rs`)

1. `main()` hands off to `cli::run` if given any CLI args; otherwise loads config and offsets (`OffsetsSource`), prints hotkeys, then polls every 100ms with `find_process::find_process(...)`, reloading the offsets when the file changes.
2. On a match, `connect()` builds a `Vec<Box<dyn Handler>>` (`create_handlers`):
   - `CutsceneTimingGeneratorHandler` **or** `CutsceneHandler` (chosen by `config.record_cutscene_timing`)
   - `OverlayHandler`
   - `PositionHandler::new_position_handler` (Lara position) and `::new_look_at_handler` (ROTTR look-at)
   - Each constructor returns `None` if the game version lacks a required `AddressType`, so features
     silently disappear per-version.
3. Registers hotkeys with `livesplit_hotkey::Hook`; keys are grouped so one key can trigger several
   `Action`s, sent over an mpsc channel.
4. Busy loop: dispatch any received `Action` to every handler's `handle_action`, then call `handle_tick`
   on all. Exits on game process exit (`is_process_running`) or SIGINT. Errors are just `eprintln!`ed.
5. Every 500ms it checks the offsets file; if the running version's offsets changed the handlers are recreated
   (losing their state), and if the game no longer matches any version it disconnects.

## File index

Each module's `//!` doc says what's in it and how it works; this is where to look.

| File | What's there |
|---|---|
| `src/main.rs` | Entry: process polling, handler construction, hotkeys, the action/tick loop; hands CLI args to `cli` |
| `src/cli.rs` | The command line tools as `clap` subcommands (some hidden), `export-offsets`, `dump` |
| `src/config.rs` | `tomb-helper.json`: hotkeys, cutscene files, the overlay's starting settings |
| `src/action.rs` | `Action`: everything a hotkey can be bound to |
| `src/handler.rs` | The `Handler` trait every feature implements |
| `offsets.json` | **Per-game-version offsets** (built into the exe), layouts, witnesses and `words` |
| `src/offsets_file.rs` | The offsets file's JSON, `OffsetsSource` (the file next to the exe overrides the built in one, hot reloaded) |
| `src/process_details.rs` | Types: `AddressType` (the offsets file's keys), `ProcessDetails`, `VersionIdentifier` |
| `src/find_process.rs` | Finding the game process and which version it is |
| `src/game.rs` | `Game`: how the overlay and `check` both find things (Lara, her proxies, probes, ...) |
| `src/game_memory.rs` | Reading the game: `Handle` (live, recording, snapshot), pointer size, readers, `*_at` accessors |
| `src/layout.rs` | `Layout`: where things are in the game's structures, per version, ROTTR Steam's by default |
| `src/words.rs` | The offsets file's `words`: names and class name parts the tools go by |
| `src/rtti.rs`, `src/components.rs` | MSVC RTTI (32/64-bit), and components found by RTTI class name |
| `src/object_list.rs` | Instances: positions, names, transforms (`to_world`), model bounds |
| `src/collision.rs` | Lara's collision: hit capsules, ground probes, her proxy and attached proxies |
| `src/collision_mesh.rs` | The level's collision meshes near a point, and raycasts against them |
| `src/aim_sweep.rs` | TR2013's aim sweep (weapon rod) through her inventory to the weapon's tuning |
| `src/trigger.rs` | What an instance is (interactable, trigger, NPC), trigger categories and shapes |
| `src/trigger_scripts.rs` | What sets triggers off and what they do, from a background scan for script nodes |
| `src/overlay_handler.rs` | Injects the overlay and sends it what to draw each tick; `overlay-data` |
| `src/inject32.rs` | Injecting the 32-bit overlay into a 32-bit game |
| `src/ui_ipc_data.rs` | The shared memory: `UiData`, `UiSettings`, double buffering (shared with the DLL) |
| `src/lib.rs` | The injected overlay (DLL): its window and drawing |
| `src/position.rs` | `Position`, with `Vector3` conversions (shared with the DLL) |
| `src/position_handler.rs` | Save/restore position, flight mode and movement |
| `src/tracked_memory.rs`, `src/tracked_position.rs` | Cached values at pointer chains, for the position and cutscene handlers |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR) and time saved |
| `src/cutscene_timing_info.rs`, `src/cutscene_timing_generator_handler.rs` | The cutscene timing file, and recording it with LiveSplit |
| `src/readable_from_path.rs` | JSON from a path or URL |
| `src/check.rs` | `check`: reads what the overlay does and checks it makes sense |
| `src/snapshot.rs` | `snapshot`/`check-snapshots`/`accept-snapshots`: `check` without the game |
| `src/discover.rs` | `discover`: the overlay's statics through RTTI |
| `src/update_offsets.rs` | `update-offsets`: the one command for a new build |
| `src/fit_layout.rs` | `fit-layout`: searching for layout offsets `check` passes with |
| `src/derive.rs` | `derive`: offsets for a new build by matching code references, `trace`, `check-matching` |
| `src/normalised.rs`, `src/witness.rs` | Matching references between compilers, and witness instructions between builds |
| `src/code_trace.rs` | Tracing how code uses a static: the pointer chains it accesses |
| `src/chains.rs` | `chains` and `probe`, live |
| `src/watch.rs` | `watch` (and hidden `watch-raw`, `disasm`): hardware breakpoints for the instructions accessing values |
| `src/layout_witness.rs` | `watch-layout`: witnesses for layout fields, matched into new builds |
| `src/pe_image.rs` | `PeImage`: an exe or dump in memory layout, or dumped from a process |

## Common changes

- **After changing code that reads the game, or the offsets file:** `tomb-helper check-snapshots` (snapshots of each
  game in `H:\tomb-helper-dumps\snapshots`, taken with `tomb-helper snapshot`) shows any game whose `check` report changed.
- **Differences between games:** put them in the `layout` (an `Option` that's `null` for what a game doesn't have),
  or detect them from the game itself (e.g. which RTTI classes its image has), rather than checking the pointer size
  or which game it is; read memory through `game_memory` and RTTI through `rtti`, which handle 32 and 64-bit.
- **New patch / game:** `update-offsets` (plus `--from` the old exe for cutscene offsets), then fix that version's
  `layout` until `check` passes (README "Porting the overlay"). New fields read from the game go in `Layout` (with
  ROTTR's value as the default), not constants.
- **Support a new game version:** run `tomb-helper derive <known exe> running` to get most of the entry, then add it to
  `offsets.json` (test it first in `tomb-helper-offsets.json` next to the exe, which hot reloads). Use `image_size`
  when the version string is shared between stores. Only include the `AddressType`s you've found — handlers
  needing missing ones won't load.
- **Add a new memory value:** add an `AddressType` variant, add offsets per version in `offsets.json`, read it with
  `Game` (`read_floats`, `require` and `game_memory`'s readers), or `TrackedMemory` in a handler that writes it.
- **Add a hotkey action:** add a variant to `Action` (`src/action.rs`), handle it in the relevant
  handler's `handle_action`, optionally add a default in `default_hotkeys()` (`src/config.rs`), and
  document it in `README.md`.
- **Add a new feature module:** implement `Handler`, add `mod` in `main.rs`, construct it in `connect()`.
- **Send more data to the overlay:** add a field to `UiData` (`src/ui_ipc_data.rs`), fill it in
  `OverlayHandler::handle_tick`, read it in `TombHelperHud::render` (`src/lib.rs`). Both binaries must be
  rebuilt together.

## Game memory notes (ROTTR Steam, Foundation/cdc engine)

Everything below was found in ROTTR Steam (`(Steam) v1.0 build 0.0_64`, image size 58654720), the only version with
the overlay offsets (`CameraView`, `CameraLens`, `InstanceArray`, `PlayerProbes`, `CollisionScene`; the layout
defaults in `layout.rs` are ROTTR Steam's too). All the RTTI classes used exist in SOTTR 492 under the same names. Offsets are hex, `mod+` means relative to the exe's base. Other versions will need them found
again (`derive` plus live checks).

### Finding your way around
- MSVC RTTI is present: an object's first qword is a vtable, `vtable[-1]` is the CompleteObjectLocator whose type
  descriptor (+0xC RVA, name at +0x10) gives the class name. Enumerating every vtable in the image and scanning memory
  for them gives every live object of each class (Python + numpy over ReadProcessMemory takes seconds).
- Objects are *instances* in a static array (`InstanceArray`, mod+0x24EEAD0, 0x120 byte slots, 8267 of them, all
  with header 2, then zeros). A slot is used if +0xE0 (TransformComponent) is set. +0x10 position and +0x30 Euler
  rotation (radians, Rx*Ry*Rz) are live; +0x78 -> placement record (+0x48 short name, +0x50 full name; null for
  spawned instances like Lara); +0xD8 -> component table with a fixed slot per component type (`components` learns
  them by class). Components are heap objects whose +0x8 points back at the instance. Which slot Lara uses changes
  when she's reloaded; she's the instance with a PlayerComponent (also Player+0x31A8, [0x100C6B0, 0, 0x31A8, 0]).
- `TransformComponent` +0x30 is a world matrix, but it isn't kept up to date for everything (Lara's stays where she
  loaded in), so the overlay only takes its scale. Its +0x80 -> Lara's live bone matrices (world space, bone n of a
  shape is matrix n + 1).
- Model bounds are in each `Mesh` resource header ("Mesh" magic, +0x10 sphere centre, +0x20/+0x30 box min/max, +0x50
  sphere radius), via DrawableComponent+0x98 -> InstanceDrawable+0xC8 -> CommonRenderModelInstance+0x48 ->
  CommonRenderModel+0x50. Terrain meshes are in world space, others object space. Undrawn instances (triggers,
  markers) have no InstanceDrawable. Every base camp has one root instance whose full name contains
  `campsite_base_trx-`, at its fire.

### Camera
- `GameRender` (`[0x2E1BD08]`): +0x30 near/far, +0x40 and +0x48 fov/aspect, +0x70 camera-to-world (CameraSin/Cos are
  its first row), +0xF0 projection, +0x520 world-to-view. Matrices are row-major for row vectors (read them
  column-major into nalgebra), view space y points down the screen, world is Z up.
- +0xF0 is swapped for a narrower fov pass for ~1ms of every frame, so the overlay builds its own projection from
  +0x30 near/far and +0x48 fov/aspect (`CameraLens`); +0x48 never showed the other pass's fov.

### Lara's collision
- **Movement:** her `cdc::ProxyComponent` capsule. It doesn't reach the ground (ground probes do that) and the shape
  in use (+0x58 radius, +0x5C *full* cylinder length, +0x60 offset from her feet in her space, +0x80 tilt, +0x90
  facing, +0x30 world centre) eases over a few frames towards the shape it's changing to (+0xC0 radius/length,
  +0xD0 offset, +0xF0 tilt). The physics capsule actually swept is proxy+0x148 (body) -> +0x20 (shape: +0x0 centre,
  +0x20 rotation, +0x44 radius, +0x48 length, +0x98 type 1, +0x99 touching flag); its volume formula
  (4/3 pi r^3 + pi r^2 h) is what shows the length isn't halved.
- Shapes seen (radius, length, offset, height above her feet): standing 25/60 (0,-5,85) 30..140; aiming 25/40
  (0,-10,90) 45..135; crouched 23.7/35.8 (0,0,60); rolling 45/15 (0,-23,90) tilted ~25deg 38..142, after a 25/50
  (0,-20,92) wind up; once 25/25 (0,-25,0) tilted 90deg (unidentified). Her local -y is (probably) forward.
- **Attached proxies:** bodies are in a list (+0x30 next, +0x48 parent, +0x50 -> owning proxy+0x120). Attached to
  hers are the camera (radius 30, length 30, offset (0,-15,90), moving to (0,30,90) i.e. behind her when aiming) and
  her weapon (radius 3.5, 90 long rod in front of her at shoulder height). They only collide while she aims, which
  is why she's held further from walls then (the rod lines up with it, including at angles).
- SOTTR (234.1) only sets the weapon rod up near something it could hit: aiming in the open, its physics shape's rotation
  is just her facing (yaw only), so it's upright inside her (hip height) and nothing shows; aiming at a wall up close it's
  level at shoulder height, 90 long, in front, as in ROTTR. Same layout and code; checked with `overlay-data`, which
  also prints her proxy's contacts.
- **What's pushing her:** a shape's +0x99 is 0x2F while it's touching something: always for Lara's, always while
  aiming for the camera (it overlaps her capsule, so this is also the best "attached proxies are on" signal), and
  only against walls for the weapon. Lara's proxy's contacts (counts +0x164/+0x168, 0x60 byte records from +0x190,
  world point first, +0x10 looks like the push) include those of the attached proxies, so a contact on a capsule's
  surface means it's pushing her. Reads are often mid update, hence the overlay's 150ms hold. Lara's proxy +0x18 bit
  0x20 clears while aiming *and* rolling, so it's not an aiming flag.
- **Wall clip theory** (speedrun glitch: back to a wall, aim, crouch/roll, twist the camera): the roll swaps a radius 25
  capsule for a radius 45 one offset 23 along her local -y, and aiming makes her face the camera, so twisting it
  swings that offset capsule; one starting inside a wall is likely what lets her through. Not confirmed in code.
- **Hit capsules:** `CollideComponent`+0x70 lists 0x18 byte entries (count +0x78): +0x8 id (byte 2 is the type, 0x8A
  capsule, high u32 + 1 is the bone), +0x10 -> shape in bone space (+0x0 centre, +0x10 quaternion, +0x28 radius,
  +0x2C length). 10 `DefensiveCapsule_*` plus attack spheres. Slow to draw, off by default.
- **Ground probes:** `PlayerProbes` (mod+0x1A8C720) is a count, then 0x60 byte records from +0x70 (+0x10 hit, +0x20
  start, +0x30 end): rays per foot and one for her body, then other characters' (two per foot). Hits far from their ray
  are stale. Hers aren't always in it: once paused in the valley hub it held only four NPCs' (8 probes, none within
  250 of her); moving, she had hers (12-16 probes, the nearest starting ~35 from her).
- **How it was found:** `watch XPosition --write` -> the player move (+0x83B330, adds a delta to her position) <- 55
  callers, the main one +0x840460 which collides via +0x82B770 on the proxy; the half length's only reader is the
  easing at +0x1DD0B0, which sets the physics shape's radius/length (+0x193FD0/+0x193F80).

### Interactables and triggers
- An instance's component table (+0xD8) has a fixed slot per component type, but many instances' tables are shorter
  and the slot then holds something else, so `components` checks the RTTI class and back pointer of what's there.
  ROTTR's slots (u64 index, for reference; they're learnt at runtime): 8 Drawable, 10 Collide, 12 TriggerComponent, 14
  cdc::ShapeComponent, 18 InteractableComponent, 30 Health, 31 Npc, 34 cdc::ProxyComponent, 64 LootComponent, 76
  Corpse, 84 CheckpointComponent, 111 CollectibleComponent. Type ids are static u32s round mod+0x1101D00.
- Interactable: InteractableComponent (campfires, search markers, tethers, throwables, corpses to loot), LootComponent
  (pickups, documents, relics, survival caches), CollectibleComponent (documents, relics, cave markers). Resources to
  gather (herb/berry plants, mushrooms, feathers, coins, saplings) are spawned instances (`craftable_resource_*`) with
  Interactable + Loot, next to a placed `craftable_resource_spawner_*` that has neither. Characters and animals have
  Interactable + Loot too (looting), and so do the places NPCs/animals play animations at (`animhost_npc_*`), which
  `trigger::for_lara` leaves out by name. Unsure: `ll_chair_vh`, `tr2_lookat_interact_parent`.
- Triggers (~920 placed `triggervolume_*`/`triggerplane_*`/`triggersphere_*`/`triggerpolyvolume_*`/`streaming_*`):
  ShapeComponent +0x18 -> 0x38 byte shape record in instance space, u32 type: 1 box (+0x8 three sizes), 3 plane (+0x8 two
  sizes, the x/z plane, i.e. upright), 4 sphere (+0x8 radius), 5 polygon volume (+0x8 u32 count, +0x10 -> 0x20 byte entries: corner
  x, y, z, 1 then the vector it's swept along, e.g. (0, 0, 1000); +0x18/+0x1C two more floats, unknown). Sizes are half sizes:
  the TriggerComponent keeps world bounds at +0xC0/+0xD0, twice the size plus 25 each side for boxes and spheres, and
  2x for planes, which shows they're upright (a 200 x 200 plane's bounds are 400 x 5 x 400). The overlap test (SOTTR
  mod+0x46EBC0, called by +0x47C390) treats the thing tested as its position and a point 60 (or its own height, [[+0x60]
  +0x20]+0x8) above, and returns which of the two are inside (1 both, 3 the bottom, 2 the top): boxes against the
  half extents (+0x40), spheres by distance (radius +0x3C), polygon volumes via +0x470330; planes never overlap there,
  so whatever sets off the ~155 plane triggers (unnamed `triggervolume`s, a `checkpoint`, `triggerplane_camera_in*`)
  is elsewhere, probably a crossing test. Slot 14 holds other things on non-triggers.
- What sets a trigger off isn't in it either: TriggerComponent +0x38 (ROTTR +0x48) is runtime state (0x40 enabled, 0x10/0x20
  moved/turned this frame, 0x08/0x04 something inside; set by SOTTR mod+0x47EA10) and every trigger is tested against the
  same global list of things (mod+0x47C390 -> +0x46EBC0, geometry only). It's decided by the ActionGraph nodes using the
  trigger, which hold its instance's u64 id (instance +0x60): AGPlayerTriggerNode (Lara), AGInstanceTriggerNode (with
  the instance's id in it too, e.g. a roaming musician's animhost), AGTriggerNode/AGTriggerMasterNode. A node's
  references are (id, other id, u32 -1, u32 type << 16 | flag): type 2 or 0xE for triggers, 3/0xF for others; no
  enter/exit event is stored per trigger. In SOTTR's village 414/477 triggers had a node: 382 player only.
- Scripts (SOTTR, and the same in ROTTR Steam; `layout.script`): no names or tags anywhere, everything is a 64 bit hash, and the ones in scripts are
  instance ids (instance +0x60; +0x70 is shared by many instances, a parent or unit). A node's +0x8 -> its graph (no
  RTTI, one per instance with a script, the owner's at instance +0x58; graph +0x8/+0x10 link every graph in a list,
  +0xA8 is its id = the owner's instance id). Graph +0x78 -> its nodes (pointers, with null gaps), +0x90 -> links header:
  u32 node count (indices are below it), u32 link count, +0x10 -> links of (u16 from, u16 to, u16 output, u16 input).
  A trigger node has a second vtable at +0x28 (only the start is in the graph). Subgraphs are AGSubGraphNodes in the
  same graph that nothing points back at, so their contents aren't followed. AGObjectNode +0x50 and AGMessageSendNode
  (and AGMessageReceiveNode) +0x28 hold the id of the instance they act on; AGInstance_ActionGraphInstanceNode +0x50 is
  its own graph's id. Following links, 390 of the 593 trigger ids in SOTTR's village lead to something recognisable.
- Nothing in the TriggerComponent says what a trigger is for, so `trigger::categorise` goes by words in its name and
  its parent's (e.g. `bendy_bv_young_pine.triggersphere`). About a third are unnamed `triggerplane`/`triggervolume`s
  in the level/mission masters, which end up with anything else unrecognised as unknown.
- Characters and animals: slot 31 NpcComponent (hares, squirrels, chickens; 32 SquadController, 33 Controller), or slot
  30 HealthComponent plus 76 CorpseComponent (birds have no NpcComponent; Lara has health but no corpse; breakables like
  `destructible_monitor` have only health, `puppet_corpse_*` only a corpse). Checked: animals, friendly Remnant (`rem_smg`,
  `remnant_parent`, `remnant_female_smg`), `jacob_companion` and Trinity (`enm_rifle`, `enm_blade_pistol`). People's
  DrawableComponent model is just a tiny part of them (3.6 x 4 x 3 at head height), so NPCs are boxed (in their own space, so the box turns with them) round their hit
  capsules (CollideComponent, like Lara's: 11 capsules + 4 spheres for people, 2 capsules for animals) instead.
- Spawned instances have no placement, but are named after their object type: instance +0x80 -> +0x8 -> +0x58 -> +0x8
  (`trx_mountain_hare`, `laracroft_jacket_light`, `wp_bow_recurve`, `craftable_resource_feathers_nestdestructibles`).
  `object_list` includes them.

### Level collision
- Static collision is triangle meshes, separate from the render meshes. `cdc::CollisionSceneClient` lists their
  instances: `CollisionScene` [mod+0x2D57440 (a `TR9MultibodyClient`, 420 code refs), +0x10, 0] -> +0x28 first instance,
  +0x30 u32 count (5068 in the Siberia gulag area), instances linked by +0x88 (+0x80 the other way).
- Instance (0xB0 bytes): +0x0 row-major object-to-world matrix (can be scaled), +0x40/+0x50 world bounds min/max,
  +0x68 -> broadphase entry (bounds, then +0x28 back to the instance), +0x70 -> mesh, +0x90 u16 bit 0 swaps the first two
  corners of every triangle, +0x98 optional material override.
- Mesh: +0x10 -> vertices, +0x18 -> 0x10 byte triangles (3 u32 vertex indices, +0xC flags, 0x40 = skipped by the
  collision tests, +0xD material index), +0x20 -> 12 byte materials (byte 0 a collision mask, always 3 here), +0x28 ->
  BVH (0x1C byte nodes: min, max, +0x18 u16 first child/triangle, +0x1A u8 triangle count, 0 for inner nodes), +0x34 u32
  triangle count, +0x38 u32 vertex count, +0x44 u16 vertex format (1 = 3 f32s, which is all that's been seen; 2 = 3 f16s).
  Floors are wound so their normal points up.
- +0x60 -> collision filter (shared, e.g. one static default record for the level), +0x4 u32 of which kinds of
  collision use it. Sampling Lara's proxy contacts for a minute while walking into things only ever found 0x0002, 0x204E,
  0x2C7F (the default), 0x0CDF and 0x000E, all with bit 1, and never 0x1C10, 0x2040 or 0x0008. Objects often have a
  detailed mesh (0x1C10, every log of a pile, probably projectiles) and a rough one (0x204E) in the same place; ~900
  instances are 0x0002 only (invisible walls?). Counts in the gulag area: 2C7F 1821, 0002 ~900, 2040 717, 0008 688, 1C10
  363, 204E 216, 0CDF 200. The overlay draws those with bit 1 as coarse, the rest as detailed (purple), each toggleable.
- Many instances are a second copy of an identical mesh in the same place, with a different filter (e.g. 0x2C7F and
  0x2041), so `collision_mesh` also drops identical ones among those it draws. ~570
  belong to objects: a `cdc::MeshInstanceComponent` (+0x8 -> its instance) holds one inline at +0x190, pointed to from
  +0x170 (e.g. bendy trees). The rest is the level.
- How it was found: `GeomTriangle`'s vtable is used by +0x18C5F0, the leaf test of a mesh query (query +0x40 ->
  instance, +0xD0 triangles, +0xD8 materials), which decodes corners with +0x18C500 / +0x18C330. Its caller +0x191C90
  walks the BVH. `JobHarness_JobMeshCollision` (+0x18B050) is the job running them.
- Physics props (barrels, breakables, worklights, bendy foliage, ...) aren't in it: they're bodies like Lara's proxy
  (0x70 byte pool, +0x30 next, +0x38 previous, +0x20 -> shape, +0x50 -> owner whose first qword is the instance), with
  shape type byte +0x98: 0 box (+0x20 quaternion, +0x30 half extents?), 1 capsule, 3 (grass, bushes; unknown), 4
  cylinder (+0x50 -> `cdc::GeomCylinder`). 1287 bodies; 1095 live `cdc::GeomHull`s are probably theirs too. Not drawn yet.

### TR2013 (32-bit)
- Builds on disk: original `v1.01.748.0` (`D:\SteamLibrary\steamapps\common\Tomb Raider`, image size 38739968) and current
  Steam `v1.01.0.0` (`G:\SteamLibrary\SteamApps\common\Tomb Raider`, 38543360). Steam was rebuilt with a newer MSVC
  (`.gfids`/`_RDATA` sections, ebp frames, getters not inlined), so byte matching finds nothing between them; `normalised`
  matching does (`check-matching`: ~90% of sure matches round trip exactly).
- Camera: a singleton, 748 `[0x211D698]`, Steam `[0x20CCDC0]` (same accessor: `mov [g], eax; call create; mov eax, [g]`). Its
  code copies `+0x460..` to `+0x50..`, so Epic's `[.., 0x460]` and 748's `[.., 0x50]` are the same values (checked live:
  Steam `[0x20CCDC0, 0x50]` = `+0x460` = the table's camera). The Steam entry's `[0x150C8C, 0x4C4]` works but by accident:
  mod+0x150C8C is in `.text`, the relocated address operand of an instruction, pointing at the static mod+0x10C4E9C, so
  it's really the static camera `0x10C5360`/`0x10C5364`, which is what `chains` finds. Steam Version is `0x107C898`.
- The exe is relocated (ASLR; base was 0x380000), so absolute addresses read live need the real module base.
- Position: a record in an array of instance-like records (748: `idx * 0x180 + mod+0xF39DF0`, pointer at +0x28; Steam:
  `idx * 0xB0 + mod+0xA786D0`, record 3 +0x10), so it can't be derived, only found live. Steam `chains` found
  `[0x20C96D8, 0x8, 0x0, 0x230C, 0x0]` (25 code sites), still right after a restart, so it's in the offsets file.
- Overlay structures in Steam TR2013 (found live, game base 0x380000):
  - Camera: `GameRender` (RTTI, same class as ROTTR) at [mod+0x20CCDC0]: +0x30 near, far, fov, aspect (just 4 floats, so
    `layout.camera.lens_*` = 0..3), +0x50 camera-to-world (det -1, +0x90 a copy; CameraSin/Cos are its first row), +0x4A0
    world-to-view (camera-to-world x it = diag(1, -1, 1, 1), view y down like ROTTR), +0x600 the game's projection.
    mod+0x10C5360 is a static copy of the camera rows.
  - Instances: a static array of 0xD0 byte records (mod+0x1D24910.., ~1263 in use), found as every
    `cdc::TransformComponent`'s +0x10 points into it. +0x0 position (+0x10 last frame's), +0x20 Euler rotation (+0x30
    last frame's), +0x70 -> the game object (Lara's is an `NsPlayerBase`; others `NsInstance`/`NsUberObjectBase`), +0x88
    -> `cdc::SceneEntity`, +0xA4..+0xB8 component pointers in the record itself (no component table). Lara was record 2
    (mod+0x1D24AB0), which the position chains reach. In the offsets file (`InstanceArray` [0x1D24910], layout: header
    +0xC0 (link indices, set up to the end), in_use +0x40 (-> object type, whose +0x4C -> name, e.g. "laracroft"), id
    +0x48 (not quite unique), no placement/components/transform). +0x64 -> component data: +0x8 count, 0xC byte
    entries from +0x14 (hash, -> name, -> data), e.g. InteractComponent, LootComponent, RagdollComponent,
    InventoryMemberComponent (Lara's gear), which `trigger::read_kinds` goes by when there's no component table.
  - Lara's movement proxy is a statically allocated `cdc::ProxyComponent` at mod+0x1113830 (`PlayerProxy`, layout
    `proxy`): +0x10 u32 flags (bit 0x20 clears while aiming, as in ROTTR), +0x18 -> its instance, +0x20 centre, +0x48
    radius/length (25, 60; aiming 25, 40), +0x50 offset (0, -5, 85; aiming (0, -10, 90)), +0x70 tilt and +0x80 facing
    quaternions, +0xA0/+0xB0/+0xD0/+0xE0 the shape it's changing to, +0xC0 her feet (feet + facing * offset = centre,
    checked), +0xF8 -> what it's attached to (her instance). No physics body (`proxy.body` null). Contacts: u32 count at
    +0x120 (the records keep their old values when it's 0, which is why they looked stale), 0x50 byte records from +0x130
    (normal + distance, then the point at +0x10). Aiming at a wall she faced, every live contact was on the combat
    proxy (`collision::read_proxy`'s `standalone`), none on hers.
  - The combat proxy is another `cdc::ProxyComponent` in the same static pool (0x7B0 apart; it was at mod+0x11175B0 one
    session and mod+0x1113FE0 the next, the slot zeroed), found through Lara's proxy +0x788 (`CombatProxy` [0x1113FB8,
    0x0]; its own +0x788 is null, so maybe a list of attached proxies), belonging to the "combatproxy" instance and
    attached to Lara's (+0xF8): radius 30, length 30, offset (0, 30, 90), with her facing. Its
    flags' bit 0x2 (`proxy.collides`) is set while she aims. The wall she faced while aiming was at local +y, so +y is
    forward then, and it's in front of her, pushing her off walls she faces (it only seemed to with some walls, probably
    a collision filter). One time not aiming, the game camera was at local +y, but the camera can face her. Those are the
    only two `ProxyComponent`s live even while aiming the bow; ROTTR's "camera" proxy (moving to (0, 30, 90) when
    aiming) may be the same thing. Correction: the user sees it *behind* Lara, and her local -y is forward (the aim
    sweep below points along -y), so the contacts were from walls at her back.
  - The aim sweep (what holds her off walls she faces while aiming, ROTTR's weapon rod): mod+0x5C5100 (found by `watch
    XPosition --write`: the writer +0x57DF7F is called from it about half the frames) sweeps a sphere (query vtable
    0xCE3078, collision filter 0x2C1AC7) through [`TR9MultibodyClient` (mod+0x1DDBAEC)]+0x10, which is the collision
    world after all (query functions +0x1EC6E0, +0x1EAE80), and pushes her back by the overlap, up to a maximum. Its
    shape is the equipped weapon type's tuning block ([type record+0x10]+0x298): start +0x0, middle +0xC (swept to it
    less the radius in y), weapon offsets +0x18/+0x24, a mirrored set from +0x30 (flag from [[player+0x2324]+0x10]
    +0x7690), radius +0x60, max pushes +0x64/+0x68; in her space, moved by her rotation and position and an extra
    offset (+0x56F940 on player+0x2E80). The bow: (-10, 0, 125) to (-20, -95, 125), radius 5. Found with `watch-raw
    --exec` on +0x5C53E4 (esi = the block). The weapon is found as the getter +0x56AE20 does: player
    ([[instance+0xA8]+0x14], RTTI `Player`) weapon slot ([[player+0x2324]+0x14] +0x1B8 if +0x1C7) or +0x30D8, else
    (as with the bow) Lara's handle (+0x50) -> `InventoryPool` (mod+0x1F6FA90) -> 0xC0 byte record, whose map (+0x20
    buckets, +0x2C count) has the item keyed by +0x64 -> its instance handle (+0x20) -> the instance's type handle
    (+0x50) -> `TypePool` (mod+0x1EFEED0) -> 0x250 byte record (bow: record 4, wp_bow_zingler). Pools: 0xBB8 0x10 byte
    handle entries (record index, id at +0x8), records from +0xBC10, each with its index and id. Instance handles index
    from mod+0x1D249E0 (one record after `InstanceArray`), checked against +0xC8/+0xCC. Only the first point set is
    drawn (no offsets). The push is added (+0x559B50) to the player's movement by kind, the third at player+0x540
    (x/y nonzero only while pushed), which the overlay shows as pushing. The static byte mod+0x20CBF31 is set while
    the sweep hits something (the next one is made longer), but isn't updated when the first sweep misses, so sticks.
  - Level collision: [`TR9MultibodyClient` (mod+0x1DDBAEC)]+0x10 is the collision world (no RTTI; `CollisionScene`
    [0x1DDBAEC, 0x10, 0]): +0x10 -> first instance, +0x14 count (2109), +0x18 -> broadphase (+0x10 leaves of 0x30: world
    min, max, +0x24 -> instance; +0x20 tree of 0x40 nodes: min, max, 4 children). Instances are 0xB0: +0x0 row-major
    matrix, +0x50/+0x60 world bounds, +0x70 -> a per area record (65 for the level, not a filter), +0x74 -> broadphase
    leaf, +0x78 -> mesh, +0x7C previous, +0x80 next, +0x84 u16 (2 for 2044 object meshes, 3 for 5, 0 for the 60 level
    meshes; not a collision filter: Lara stands on the 0 ones). The level meshes' vertices are relative to an origin at
    +0x40 (`collision_instance.origin`; zeros for the rest, and the same as their area record's +0x0), added after the
    matrix, which is the identity for them; their bounds at +0x50 don't include it either. Without it the ground (e.g. a
    105824 triangle mesh, 30000 x 30000) was missing and drawn somewhere else, looking like structures from elsewhere.
    No collision filter was found, so every mesh is drawn as Lara's. Meshes: +0x0/+0x10 object space bounds,
    +0x20 -> vertices (inline after the 0x44 byte header), +0x24 -> 0x14 byte triangles (3 u32 indices, +0xC flags e.g.
    0x00A0033F, +0x10 another u32), +0x28 -> 0x1C byte BVH nodes (+0x2C count), +0x30 triangle count, +0x34 vertex
    count, +0x40 u16 format (1 = floats). Wound with floors up (no flip flag found; `collision_instance.flags` null).
    Every mesh's vertices through its matrix give exactly its world bounds. Found from the aim sweep's query (+0x1EC6E0 ->
    +0x131900 -> the broadphase's +0x1EB5C0).
  - Dead ends while looking for the ground: the terrain classes (`JobHarness_JobTerrainNodeIntersections` +0x2BC770 is
    visibility, marking scene cells against planes; `TerrainCallback`/`GameTerrainCallback` are drawing and lights).
    Every triangle test (`GeomTriangle` users +0x128300, +0x12F600) reads these meshes' 0x14 byte triangles (+0xC bit
    0x40 skipped, +0xD a mask against the query), all queries go to this one world, and the world's +0x8 list (369
    0x80 byte entries) is dynamic bodies (Lara's ragdoll parts).
  - Triggers: instances whose game object (instance +0x74, `instance.game_object`; +0x70 is something else) is an
    `NsTriggerVolumeBase` (`triggervolume`, `_dry`, `_fire_snuff`, `_waterfall`, `_splash`) or `NsTriggerPlaneBase`
    (`triggerplane`, `_rain`); 561 in the first area, and nothing else is named trigger. Placed by the instance's position
    and Euler rotation (the object's +0x40 quaternion matches). Volume (`trigger_object`): +0x80 u32 type, +0x84 bounding
    radius, +0x88 three sizes, set up by +0x19D720 from the placement data (type byte +0x18, sizes +0x1C, type 2's
    radius +0x28): 1 box (half extents; the radius is their length), 2 sphere (the three sizes are equal, and +0x79F60
    uses only the first for types 2 and 3; its bounding radius is bigger). Plane: +0x40 half width, half height, then
    their length. Assumed upright in the x/z plane like ROTTR's; not checked against the game. The component data at
    +0x64 is garbage for planes.
  - Trigger scripts (`trigger_scripts`, TR2013's `script` layout): there are no trigger nodes. Scripts use a trigger like
    any instance, through an `AGInstanceNode` (or `AGObjectNode`) with its id (u64, instance +0x50, nearly unique; the
    instance layout's `id`) at +0x40, whose outputs are its events (4, 5 and 6 nearly always; which one is Lara isn't
    known, so they're all "script"). Node +0xC -> its graph (the level's, e.g. the area master's), graph +0x30 -> node
    pointers (with gaps), +0x40 -> definition: +0x0 node count, +0x4 link count, +0xC -> links (u16 from, to, output,
    input, as in ROTTR). An instance's own graph is at instance +0x70 (`GameInstanceData`). AGMessageSendNodes don't
    hold the instance's id. 223 of the 561 triggers in the first area are used by a script (+47 through AGObjectNodes);
    the rest only by their own data (the effect volumes and many plain ones). Nodes they lead to: UnitStreamLayerNode
    (streaming), AGBattleConductor_*, AGNPC*, AGHuntManager_*, GameObjectiveNode, GameTutorialStep, MiscPersistentData,
    AGSoundPlex/Reverb/SFXMarker, AGInstanceGiveBirth/Destroy/Teleport, AGCamera_*.
  - Ground probes: `PlayerProbes` mod+0x1B21580, found by `update-offsets` by their contents, with ROTTR's layout (count, 0x60
    byte records from +0x70: +0x10 hit, +0x20 start, +0x30 end): two rays by her feet and one up through her, all hitting
    the ground under her. Each starts a little below the ground and ends above it (ROTTR's go down), which only swaps
    which end is which when drawn.
  - The instance array's header (+0xC0) differs from slot to slot (and slot 0's +0x80 is 0 where the rest are -1), so
    `discover` looks for runs of nonzero headers and checks them by their objects' names.
- v1.01.748 has the overlay too, all found by `update-offsets --from` the Steam exe (with that install's
  `steam_appid.txt`, 203160, so it runs from D: rather than Steam starting its own copy): `check` passes everything that
  runs. Its layout is Steam's but for Lara's proxy, a static at mod+0x117D200 that points at her instance through +0x10
  (Steam +0x18, which `update-offsets` found and moved itself). Its proxies' flags are at +0x8 (Steam +0x10), found
  by sampling while aiming: the combat proxy's bit 0x2 is set and Lara's bit 0x20 clears while she aims. The Steam build's layout witnesses don't match into it (a different compiler, and witnesses match by
  bytes).
- v1.01.748's aim sweep works the same way as Steam's: `TypePool` is the weapon pool (mod+0x1F54FE0), a record per
  weapon *instance* keyed by its handle (instance +0x50, which is its own handle in both builds), whose data pointer is
  at +0x8 (Steam +0x10), tuning block at data+0x298. A rifle has two instances, only one of them with its real tuning
  (the other, like the axes, has a default (0, 0, 100) to (0, -100, 100) r10), so the tuning has to come through the
  instance, not its type: the instance's +0x64 is shared by both and led to the default for the rifle. Its handle
  entries are (record, flags, id, 0), not (record, -1, id, 0) like the inventory's, which hid it from a first search.
  The inventory pool's records start 8 bytes before `aim_sweep.records` (0xBC10, which both pools share), so its
  fields are 8 earlier in the layout. The player is [[instance+0xA4]+0xC] (Steam [[+0xA8]+0x14]); the push at +0x540
  isn't checked. Weapons in both builds: pistol (-10, 0, 120) to (-33, -70, 120) r8, shotgun (-7, 0, 110) to (-27,
  -70, 110) r7, rifle (-10, 0, 120) to (-30, -75, 120) r5, bow (-10, 0, 125) to (-20, -90, 125) r5.
- Live tools all work on Steam TR2013: `probe`, `chains`, `update-offsets` (finds every static but the two pools, which `--from` derives), `watch`/`watch-raw` (debug
  registers set in the native context, registers read from the WOW64 one; 800 traps in 2s on the QueryPerformanceCounter
  import slot). Lara's position and the camera weren't read at all while the game was unfocused (paused).

### Working with the live game
- The user plays; ask them to switch to the game before anything that needs it running (it pauses when alt-tabbed),
  then start with a delay. Reads work fine while it's paused.
- Throwaway exploration has been done with Python (ctypes ReadProcessMemory + numpy) from the job's tmp dir: run it
  with `-I`, which also ignores `PYTHONIOENCODING`, so use `sys.stdout.reconfigure(encoding='utf-8')`.
- Heap addresses change when the game restarts, but the proxy/body/shape objects stayed put within a session.

## Gotchas

- Windows-only; non-Windows stubs in `find_process.rs` just panic.
- `PositionHandler` "active" mode writes the cached position every tick, which is what lets Lara be
  moved/frozen — movement actions are no-ops while inactive.
- `CutsceneHandler::skip` deliberately returns `Ok` when not in a cutscene (Space is also jump).
  A blacklist `skip_delay` of `20000.0` means "never skip".
- `PositionHandler` and `CutsceneHandler` both bind Space by default (`Up` and `SkipCutscene`).
- The games pause when alt-tabbed, so live tools that need activity (`watch`) take `--delay`.
- The shared memory is backed by a file in `%TEMP%\shared_memory-rs`, which outlives a crashed or killed exe; it's
  named after the `UiData` layout's size so a changed layout gets a new one.
- A running `tomb-helper.exe` locks the build output, so `cargo build` fails until it's closed.
- `tomb-helper.json`'s `overlay.console` turns the injected overlay's console back on for debugging.
- If the exe ends without setting `should_exit` (e.g. its window is closed), an overlay that stays loaded keeps that
  run's shared memory and draws its last data; injecting again does nothing as the DLL is already loaded, and its file
  can't be replaced. The overlay now ejects itself after 5s without an update (`EXE_GONE`); check the game's modules
  (32-bit ones for TR2013) if changes to the overlay don't show. `overlay-data [--delay N]` (hidden) prints what the exe
  would send without injecting; it owns the shared memory, so not while tomb-helper runs.
