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
| `src/handler.rs` | `Handler` trait: `handle_tick`, `handle_action`, and for the overlay's window `take_actions` (button presses, given to every handler like hotkeys), `report`/`show` (`HelperState`: the position handler's saved position and flight mode, shown by the overlay) |
| `offsets.json` | **Per-game-version memory offsets** (built into the exe with `include_str!`). Hex strings, `bits`, optional `image_size` (to tell Steam/Epic apart), optional `witnesses` (from `watch`, only used by `derive`), ignored `unverified` |
| `src/offsets_file.rs` | JSON schema (`OffsetsFile`/`VersionEntry`/`Hex`, `deny_unknown_fields` to catch typos), `OffsetsSource` (`tomb-helper-offsets.json` next to the exe overrides the built in file; `changed()` for hot reload), `known_process_details()` for the CLI tools, `to_json` (one line per offset list) |
| `src/process_details.rs` | Types only: `AddressType` enum (serde names are the JSON keys), `ProcessDetails`, `VersionIdentifier` |
| `src/derive.rs` | CLI dispatch (`dump`/`derive`/`chains`/`probe`/`trace`/`watch`, hidden `watch-raw`/`disasm`/`check-matching`; flags `--version`/`--seconds`/`--delay`/`--write`/`--watch`). `derive`: fuzzy-matches code references between builds to find static addresses (`Deriver`: byte windows, or when that isn't confident `normalised`, sure only if deriving back lands within 0x400), then aligns `code_trace` accesses at matched sites to map each chain level (`code`/`witness`/`shifted`/`copied`; between compilers only an unchanged offset or a strong vote counts). On x86 a code position only counts as a reference if an instruction uses it. `check-matching <a> <b> [n]` round-trips n statics to measure matching |
| `src/normalised.rs` | Matching references between builds from different compilers: the code around each (12 instructions either side, within the function) as tokens without registers, stack offsets or addresses (strings/imports/constants by content), plus one for the referencing instruction; weighted cosine (idf), candidates from an inverted index of rare tokens, runner-up only among references to other statics elsewhere |
| `src/code_trace.rs` | `Tracer`: data-flow trace from an instruction using a static address, following pointer loads, branches, direct calls (args + returned `rax`) and, with a live `CallResolver`, virtual calls; on x86 also into the callers of a getter returning the static's pointer (direct `call rel32` index); produces `Access` chains in table form |
| `src/watch.rs` | `watch <AddressType>...`: attaches as a debugger (`DebugActiveProcess`), sets DR0-3 data breakpoints on up to 4 values (resolved after `--delay`), records accessing instructions ("witnesses") and the caller chain of each (return addresses scanned off the stack), clears and detaches. `--write` breaks on writes only. 32-bit games: debug registers in the native context, the registers the code sees from the WOW64 one (`Context`); an access by a `call [mem]` is found at the return address. Hidden `watch-raw <pid> <addr or +rva>` for any address, and `disasm <pid> <addr or +rva> [count]` (prints RVAs; call targets are absolute) |
| `src/witness.rs` | Matches witness instructions into a new build by byte-window similarity around the displacement; the displacement change gives the new last offset |
| `src/chains.rs` | Live: `chains` traces every static reference, evaluates the chains in the running game and lists code chains landing on each current value; `probe` prints current values |
| `src/pe_image.rs` | `PeImage`: load an exe in memory layout (or a dump), or dump a running process's main module (works on Denuvo builds); `sections()`, `imports()` (IAT slot -> name) |
| `src/find_process.rs` | Win32 process/module lookup (`get_pid`, `get_base_address`, `is_process_running`), version detection (image size first, else reads version string from memory), `force_version` override |
| `src/tracked_memory.rs` | `TrackedMemory<T>`: cached value + pointer-chain offsets; `fetch_from_game` / `apply_to_game`. First offset is relative to module base |
| `src/tracked_position.rs` | `TrackedPosition`: three `TrackedMemory<f32>` (x/y/z), `to_position()` |
| `src/position.rs` | Plain `#[repr(C)] Position {x,y,z}` (shared with the DLL) |
| `src/position_handler.rs` | Store/restore position, toggle "active" (freezes position by re-writing it every tick), camera-relative WASD/up/down movement using `CameraSin`/`CameraCos` |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR): checks prompt/status/timeline vs. blacklist delay, sets status=5 to skip, tracks RTA/IGT saved using timing info. Also `Blacklist` type |
| `src/cutscene_timing_info.rs` | `TimingInfo` / `TimingEntry` JSON format (cutscene ids → durations, skippable-at times) |
| `src/cutscene_timing_generator_handler.rs` | Records cutscene timings by watching memory + querying LiveSplit Server over TCP (`getcurrenttime`); writes the timing file after each cutscene |
| `src/readable_from_path.rs` | `ReadableFromPath` trait: load JSON from a local path or URL (blocking reqwest), default on failure |
| `src/inject32.rs` | Injects the 32-bit overlay into a 32-bit game: `LoadLibraryW` from the game's own 32-bit kernel32 (export table read from the game), as hudhook's injector uses this process's 64-bit one |
| `src/overlay_handler.rs` | On `ToggleUI` injects `tomb_helper_ui.dll` into the game (`hudhook::inject`), or `tomb_helper_ui32.dll` into a 32-bit one (`inject32`); the camera and instance array are optional (without them the overlay gets camera address 0 and only shows its window); each tick streams into shared memory: position, `CameraView`, a projection built from `CameraLens`, object boxes (objects matching `config.overlay` filters, rescanned every second), and Lara's collision from `collision.rs` (hit capsules, ground probes, proxy capsules, with a 150ms hold on "pushing"); sets `should_exit` to eject on toggle-off/drop. `TrackedCameraMatrix` reads a 4x4 matrix in one copy |
| `src/object_list.rs` | Reads the game's static instance array (`InstanceArray`, 0x120 byte records until a zero header): position, placement names, world matrix (built from the instance's position + Rx*Ry*Rz Euler rotation, with only the scale from `cdc::TransformComponent`, which goes stale for Lara), and model bounds (`read_bounds`: component table -> DrawableComponent -> InstanceDrawable -> CommonRenderModelInstance -> CommonRenderModel -> `Mesh` header min/max). Layout offsets are constants found in ROTTR Steam |
| `src/collision.rs` | Lara's collision, see "Lara's collision" below: hit capsules (`read_capsule_shapes`/`place_capsules`), ground probes (`read_probes`), and her movement proxy with the camera/weapon proxies attached to it (`read_proxy`), including which of them is pushing her (contacts on their surface). Has unit tests for the capsule geometry |
| `src/collision_mesh.rs` | The level's collision meshes (see "Level collision" below): `CollisionMeshes::refresh` walks the scene's instance list (dropping duplicates), `triangles_near` caches each mesh's vertices/triangles and returns the world space triangles near Lara, closest first, tagged floor/wall/ceiling by their normal |
| `src/trigger.rs` | What an instance is from its component table (`read_kinds`: interactable via Interactable/Loot/Collectible components, trigger via TriggerComponent, character/animal via Npc or Health + Corpse components; RTTI checked), resources by name (`is_resource`), trigger categories by name (`categorise`), trigger shapes (`read_shape`: box, plane, sphere, polygon volume), their world space outlines and faces (`faces`, which the overlay fills in at `trigger_opacity`; polygon caps are ear clipped as they can be concave) |
| `src/layout.rs` | `Layout`: where everything the overlay reads is in the game's structures (instance, drawable, collide, proxy, body, physics shape, probes, camera, collision scene/instance/mesh, trigger shape), ROTTR Steam's by default, overridable per version in the offsets file's `layout` section (optional offsets, e.g. TR2013's missing instance `transform`, are `null`). `layout::set_for(details)` on connecting sets it and the game's pointer size (`pointer_size()`/`pointer_from()`, which everything reading game pointers uses) |
| `src/components.rs` | Components by RTTI class name: `Classes` (vtable -> class name via the CompleteObjectLocator), `Components::get` learns each class's slot from a table (budgeted per `start_refresh`) and checks class + back pointer. Class name constants |
| `src/discover.rs` | `discover [--save]`: finds GameRender (camera), TR9MultibodyClient (-> CollisionSceneClient) and the instance array statics through what they point to, optionally writing them to the offsets file |
| `src/update_offsets.rs` | `update-offsets [--from <exe or dump>] [--dry-run]`: the one command for a new build. Attaches to any known exe (new builds start from the last known entry for it; an entry without a layout borrows the last one of the same game that has one), finds statics (`discover::find_statics`), Lara (PlayerComponent) -> position chain (kept if the previous chain still lands there, else `chains::best_code_chains`; Y/Z = X + 4/8), ground probes by contents, version string; `--from` adds `derive::derive_lines`; the rest goes to `unverified` for a new build. Saves (`discover::save_entry`) and runs `check::run`. For a 32-bit game only the version string and `--from`, then `chains::print_values` |
| `src/check.rs` | `check`: reads everything the overlay does from the running game and checks it makes sense, to port the layout |
| `src/trigger_scripts.rs` | What sets triggers off and what they do: a background heap scan for ActionGraph trigger nodes (vtables of every class found by RTTI in the image, `all_vtables`), matched to triggers by instance id, then each node's links in its graph followed (`layout.script`) to the nodes it leads to, described by class (`ACTIONS`) and the instances they act on; cached per id, rescanned for new triggers (10s) and ones with no script (60s). The overlay can show triggers by these words (`TRIGGER_ACTIONS` in `ui_ipc_data`) instead of by name |
| `src/ui_ipc_data.rs` | Shared-memory IPC (`shared_memory` crate, os_id `"TombHelperIPC-<size of UiDoubleBuffer>"` so a stale mapping from another layout is never opened): `UiData` (camera, `ObjectBox`es, `Capsule`s, `Probe`s, collision `Triangle`s), `UiDoubleBuffer` with a sequence counter per buffer, changed and read in place since `UiData` is ~7MB, copying only the triangles in use, plus `UiSettings` (atomic show/hide toggles, mesh radii, opacity, fade and hidden triangle culling, which the DLL's checkboxes/sliders and the exe's hotkeys change) (`update`; the reader uses `try_read` into a scratch box and keeps its last good copy, since the exe never waits), `SharedMemoryManager` (exe is owner) |
| `src/lib.rs` | Injected overlay: `TombHelperHud` implements `ImguiRenderLoop`; opens a console only if `config.overlay.console`; shows a position window and draws (near-plane clipped, in world space via `Projector`) a box + name per `ObjectBox`, Lara's capsules, probes, a circle at her feet and the level's collision triangles (filled faintly, under everything, without anti-aliasing,
skipping back faces and ones off screen, and optionally ones behind others via a quarter resolution software ID buffer
(`OcclusionBuffer`); the position window shows how long they took to find and draw). Colours: model boxes green, estimated yellow, collision capsules white / yellow while pushing, proxy's next shape red, probes cyan, collision floors green / walls orange / ceilings blue. Calls `eject()` when `should_exit` is set. Reads the camera itself each frame (`ReadProcessMemory` on its own process,
at addresses the exe sends) since the exe's copy lags and made everything judder as the camera turned |

## Common changes

- **New patch / game:** `update-offsets` (plus `--from` the old exe for cutscene offsets), then fix that version's
  `layout` until `check` passes (README "Porting the overlay"). New fields read from the game go in `Layout` (with
  ROTTR's value as the default), not constants.
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
  - The combat proxy is another static `cdc::ProxyComponent` at mod+0x11175B0 (`CombatProxy`), belonging to the
    "combatproxy" instance and attached to Lara's (+0xF8): radius 30, length 30, offset (0, 30, 90), with her facing. Its
    flags' bit 0x2 (`proxy.collides`) is set while she aims. The wall she faced while aiming was at local +y, so +y is
    forward then, and it's in front of her, pushing her off walls she faces (it only seemed to with some walls, probably
    a collision filter). One time not aiming, the game camera was at local +y, but the camera can face her. Those are the
    only two `ProxyComponent`s live even while aiming the bow; ROTTR's "camera" proxy (moving to (0, 30, 90) when
    aiming) may be the same thing.
  - Not found: ground probes, and the collision scene (`CollisionSceneClient` doesn't exist; `TR9MultibodyClient` at
    [mod+0x1DDBAEC] has no scene at +0x10).
- Live tools all work on Steam TR2013: `probe`, `chains`, `update-offsets` (32-bit path), `watch`/`watch-raw` (debug
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
- In a `.claude/worktrees/` checkout the `../hudhook` patch doesn't resolve: build with
  `cargo build --config "patch.crates-io.hudhook.path='H:/hudhook'"`.
- `tomb-helper.json`'s `overlay.console` turns the injected overlay's console back on for debugging.
