# tomb-helper — project index

Speedrun helper for the Tomb Raider reboot trilogy (TR2013, ROTTR, SOTTR). Windows-only Rust tool that
attaches to the running game, reads/writes its memory via pointer chains, and reacts to global hotkeys.
User docs (hotkeys, config, cutscene timing generation) live in `README.md`.

## Build outputs (two crates from one package — see `Cargo.toml`)

| Target | Entry | Output | Purpose |
|---|---|---|---|
| bin `tomb-helper` | `src/main.rs` | `tomb-helper.exe` | Main external process: finds game, runs handlers, listens to hotkeys |
| lib `tomb_helper_ui` (cdylib) | `src/lib.rs` | `tomb_helper_ui.dll` | ImGui overlay injected *into* the game (DX12 via `hudhook`) |

- The bin and lib share `src/ui_ipc_data.rs` and `src/position.rs` by each declaring them with `mod` —
  they're compiled into both, so `UiData` must stay `#[repr(C)]` with identical layout on both sides.
- `hudhook` (0.9.x) is patched to a local checkout at `../hudhook` (`[patch.'crates-io']`), which is
  upstream `main` plus a local `Process::by_pid()` in `src/inject.rs` (not upstream; used by
  `OverlayHandler`). Builds need that sibling directory. The patch's version must match the
  `hudhook = "..."` requirement or cargo silently ignores it and uses crates.io.
- CI: `.github/workflows/rust.yml` (build + test on push/PR to master), `main.yml` (release on `v*` tag,
  uploads only `tomb-helper.exe`). Versioning via `cargo-release`.
- `.cargo/config.toml` holds the `crt-static` rustflags (static CRT so the exe/dll run standalone).
  It must stay under `.cargo/` — cargo ignores a `config.toml` at the repo root.

## Runtime flow (`src/main.rs`)

1. `main()` hands off to `derive::run` if given any CLI args; otherwise loads config and offsets (`OffsetsSource`), prints hotkeys, then polls every 100ms with `find_process::find_process(...)`, reloading the offsets when the file changes.
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

| File | What's there |
|---|---|
| `src/main.rs` | Entry, process polling loop, handler construction, hotkey wiring, action/tick loop |
| `src/config.rs` | `Config` (serde), loaded from `tomb-helper.json` next to the exe; default hotkeys, default gist URLs, `CutsceneTiming::On/Off` |
| `src/action.rs` | `Action` enum — every hotkey-bindable action. Movement actions carry `distance` (default 100.0) |
| `src/handler.rs` | `Handler` trait: `handle_tick`, `handle_action` |
| `offsets.json` | **Per-game-version memory offsets** (built into the exe with `include_str!`). Hex strings, `bits`, optional `image_size` (to tell Steam/Epic apart), optional `witnesses` (from `watch`, only used by `derive`), ignored `unverified` |
| `src/offsets_file.rs` | JSON schema (`OffsetsFile`/`VersionEntry`/`Hex`, `deny_unknown_fields` to catch typos), `OffsetsSource` (`tomb-helper-offsets.json` next to the exe overrides the built in file; `changed()` for hot reload), `known_process_details()` for the CLI tools, `to_json` (one line per offset list) |
| `src/process_details.rs` | Types only: `AddressType` enum (serde names are the JSON keys), `ProcessDetails`, `VersionIdentifier` |
| `src/derive.rs` | CLI dispatch (`dump`/`derive`/`chains`/`probe`/`trace`/`watch`, hidden `watch-raw`/`disasm`; flags `--version`/`--seconds`/`--delay`/`--write`/`--watch`). `derive`: fuzzy-matches code references between builds to find static addresses, then aligns `code_trace` accesses at matched sites to map each chain level (`code`/`witness`/`shifted`/`copied`) |
| `src/code_trace.rs` | `Tracer`: data-flow trace from an instruction using a static address, following pointer loads, branches, direct calls (args + returned `rax`) and, with a live `CallResolver`, virtual calls; produces `Access` chains in table form |
| `src/watch.rs` | `watch <AddressType>...`: attaches as a debugger (`DebugActiveProcess`), sets DR0-3 data breakpoints on up to 4 values (resolved after `--delay`), records accessing instructions ("witnesses") and the caller chain of each (return addresses scanned off the stack), clears and detaches. `--write` breaks on writes only. 64-bit only. Hidden `watch-raw <pid> <addr or +rva>` for any address, and `disasm <pid> <addr or +rva> [count]` (prints RVAs; call targets are absolute) |
| `src/witness.rs` | Matches witness instructions into a new build by byte-window similarity around the displacement; the displacement change gives the new last offset |
| `src/chains.rs` | Live: `chains` traces every static reference, evaluates the chains in the running game and lists code chains landing on each current value; `probe` prints current values |
| `src/pe_image.rs` | `PeImage`: load an exe in memory layout (or a dump), or dump a running process's main module (works on Denuvo builds) |
| `src/find_process.rs` | Win32 process/module lookup (`get_pid`, `get_base_address`, `is_process_running`), version detection (image size first, else reads version string from memory), `force_version` override |
| `src/tracked_memory.rs` | `TrackedMemory<T>`: cached value + pointer-chain offsets; `fetch_from_game` / `apply_to_game`. First offset is relative to module base |
| `src/tracked_position.rs` | `TrackedPosition`: three `TrackedMemory<f32>` (x/y/z), `to_position()` |
| `src/position.rs` | Plain `#[repr(C)] Position {x,y,z}` (shared with the DLL) |
| `src/position_handler.rs` | Store/restore position, toggle "active" (freezes position by re-writing it every tick), camera-relative WASD/up/down movement using `CameraSin`/`CameraCos` |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR): checks prompt/status/timeline vs. blacklist delay, sets status=5 to skip, tracks RTA/IGT saved using timing info. Also `Blacklist` type |
| `src/cutscene_timing_info.rs` | `TimingInfo` / `TimingEntry` JSON format (cutscene ids → durations, skippable-at times) |
| `src/cutscene_timing_generator_handler.rs` | Records cutscene timings by watching memory + querying LiveSplit Server over TCP (`getcurrenttime`); writes the timing file after each cutscene |
| `src/readable_from_path.rs` | `ReadableFromPath` trait: load JSON from a local path or URL (blocking reqwest), default on failure |
| `src/overlay_handler.rs` | On `ToggleUI` injects `tomb_helper_ui.dll` into the game (`hudhook::inject`); each tick streams into shared memory: position, `CameraView`, a projection built from `CameraLens`, object boxes (objects matching `config.overlay` filters, rescanned every second), and Lara's collision from `collision.rs` (hit capsules, ground probes, proxy capsules, with a 150ms hold on "pushing"); sets `should_exit` to eject on toggle-off/drop. `TrackedCameraMatrix` reads a 4x4 matrix in one copy |
| `src/object_list.rs` | Reads the game's static instance array (`InstanceArray`, 0x120 byte records until a zero header): position, placement names, world matrix (built from the instance's position + Rx*Ry*Rz Euler rotation, with only the scale from `cdc::TransformComponent`, which goes stale for Lara), and model bounds (`read_bounds`: component table -> DrawableComponent -> InstanceDrawable -> CommonRenderModelInstance -> CommonRenderModel -> `Mesh` header min/max). Layout offsets are constants found in ROTTR Steam |
| `src/collision.rs` | Lara's collision, see "Lara's collision" below: hit capsules (`read_capsule_shapes`/`place_capsules`), ground probes (`read_probes`), and her movement proxy with the camera/weapon proxies attached to it (`read_proxy`), including which of them is pushing her (contacts on their surface). Has unit tests for the capsule geometry |
| `src/collision_mesh.rs` | The level's collision meshes (see "Level collision" below): `CollisionMeshes::refresh` walks the scene's instance list (dropping duplicates), `triangles_near` caches each mesh's vertices/triangles and returns the world space triangles near Lara, closest first, tagged floor/wall/ceiling by their normal |
| `src/trigger.rs` | What an instance is from its component table (`read_kinds`: interactable via Interactable/Loot/Collectible components, trigger via TriggerComponent, character/animal via Npc or Health + Corpse components; RTTI checked), resources by name (`is_resource`), trigger categories by name (`categorise`), trigger shapes (`read_shape`: box, plane, sphere, polygon volume) and their world space outlines |
| `src/layout.rs` | `Layout`: where everything the overlay reads is in the game's structures (instance, drawable, collide, proxy, body, physics shape, probes, camera, collision scene/instance/mesh, trigger shape), ROTTR Steam's by default, overridable per version in the offsets file's `layout` section. `layout::current()` is set on connecting |
| `src/components.rs` | Components by RTTI class name: `Classes` (vtable -> class name via the CompleteObjectLocator), `Components::get` learns each class's slot from a table (budgeted per `start_refresh`) and checks class + back pointer. Class name constants |
| `src/discover.rs` | `discover [--save]`: finds GameRender (camera), TR9MultibodyClient (-> CollisionSceneClient) and the instance array statics through what they point to, optionally writing them to the offsets file |
| `src/check.rs` | `check`: reads everything the overlay does from the running game and checks it makes sense, to port the layout |
| `src/ui_ipc_data.rs` | Shared-memory IPC (`shared_memory` crate, os_id `"TombHelperIPC-<size of UiDoubleBuffer>"` so a stale mapping from another layout is never opened): `UiData` (camera, `ObjectBox`es, `Capsule`s, `Probe`s, collision `Triangle`s), `UiDoubleBuffer` with a sequence counter per buffer, changed and read in place since `UiData` is ~7MB, copying only the triangles in use, plus `UiSettings` (atomic show/hide toggles, mesh radii, opacity, fade and hidden triangle culling, which the DLL's checkboxes/sliders and the exe's hotkeys change) (`update`; the reader uses `try_read` into a scratch box and keeps its last good copy, since the exe never waits), `SharedMemoryManager` (exe is owner) |
| `src/lib.rs` | Injected overlay: `TombHelperHud` implements `ImguiRenderLoop`; opens a console only if `config.overlay.console`; shows a position window and draws (near-plane clipped, in world space via `Projector`) a box + name per `ObjectBox`, Lara's capsules, probes, a circle at her feet and the level's collision triangles (filled faintly, under everything, without anti-aliasing,
skipping back faces and ones off screen, and optionally ones behind others via a quarter resolution software ID buffer
(`OcclusionBuffer`); the position window shows how long they took to find and draw). Colours: model boxes green, estimated yellow, collision capsules white / yellow while pushing, proxy's next shape red, probes cyan, collision floors green / walls orange / ceilings blue. Calls `eject()` when `should_exit` is set. Reads the camera itself each frame (`ReadProcessMemory` on its own process,
at addresses the exe sends) since the exe's copy lags and made everything judder as the camera turned |

## Common changes

- **Port the overlay:** `discover --save`, then `check`, fixing that version's `layout` until it passes (README "Porting
  the overlay"). New fields read from the game go in `Layout` (with ROTTR's value as the default), not constants.
- **Support a new game version:** run `tomb-helper derive <known exe> running` to get most of the entry, then add it to
  `offsets.json` (test it first in `tomb-helper-offsets.json` next to the exe, which hot reloads). Use `image_size`
  when the version string is shared between stores. Only include the `AddressType`s you've found — handlers
  needing missing ones won't load.
- **Add a new memory value:** add an `AddressType` variant, add offsets per version in `offsets.json`, read it with
  `TrackedMemory::new(default, offsets.clone(), arch, base_addr)`.
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
  start, +0x30 end): rays per foot and one for her body. Hits far from their ray are stale.
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
  sizes, the x/y plane), 4 sphere (+0x8 radius), 5 polygon volume (+0x8 u32 count, +0x10 -> 0x20 byte entries: corner
  x, y, z, 1 then the vector it's swept along, e.g. (0, 0, 1000); +0x18/+0x1C two more floats, unknown). Whether box and
  plane sizes are half sizes is unverified. Slot 14 holds other things on non-triggers.
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
- In a `.claude/worktrees/` checkout the `../hudhook` patch doesn't resolve: build with
  `cargo build --config "patch.crates-io.hudhook.path='H:/hudhook'"`.
- `tomb-helper.json`'s `overlay.console` turns the injected overlay's console back on for debugging.
