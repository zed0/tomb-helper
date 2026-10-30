# tomb-helper — project index

Speedrun helper for the Tomb Raider reboot trilogy (TR2013, ROTTR, SOTTR). Windows-only Rust tool that
attaches to the running game, reads/writes its memory via pointer chains, and reacts to global hotkeys.
User docs (hotkeys, config, cutscene timing generation) live in `README.md`.

## Build outputs (two crates from one package — see `Cargo.toml`)

| Target | Entry | Output | Purpose |
|---|---|---|---|
| bin `tomb-helper` | `src/main.rs` | `tomb-helper.exe` | Main external process: finds game, runs handlers, listens to hotkeys |
| lib `tomb_helper_ui` (cdylib) | `src/lib.rs` | `tomb_helper_ui.dll` | ImGui overlay injected *into* the game (DX12 via `hudhook`; built for `i686-pc-windows-msvc` it's DX11 for TR2013, `tomb_helper_ui32.dll`, which `build.rs` builds with a second cargo and copies next to the exe; `TOMB_HELPER_SKIP_TR2013` skips it) |

- The bin and lib share `src/ui_ipc_data.rs`, `src/position.rs`, `src/projection.rs` and `src/logging.rs` by each declaring them with `mod` —
  they're compiled into both, so `UiData` must stay `#[repr(C)]` with identical layout on both sides, including in the
  32-bit overlay for TR2013: nothing in the shared memory may be pointer sized (`u64` addresses, `u32` counts). Every
  build checks `UiDoubleBuffer`'s size is `SHARED_SIZE` (`ui_ipc_data.rs`), so a layout change fails the build until
  that's updated (the error says the new size), and a field that's another size in the 32-bit overlay fails its build.
- The overlay only hooks DirectX 12 (64-bit) and DirectX 11 (32-bit), so it doesn't appear in ROTTR or SOTTR running
  DirectX 11, or TR2013 running DirectX 9. hudhook has DX11 and DX9 hooks too, so the DLL could pick by which d3d DLL
  the game has loaded.
- `hudhook` (0.9.x) is patched (`[patch.'crates-io']`) to the `vtable-hooks` branch of github.com/zed0/hudhook,
  which is upstream `main` plus (none of it upstream): `Process::by_pid()` in `src/inject.rs` (used by
  `OverlayHandler`); the `hudhook!` macro's `DllMain` returning TRUE (without a return value the loader read a leftover
  register, and when it was 0 unloaded the DLL under the thread `DllMain` had started, crashing the game at random on
  inject; `lib.rs` has its own `DllMain` now, which does too); and `HookMethod::VTable` (`with_hook_method`), hooking
  DirectX 11/12 by swapping vtable slots rather than patching the functions, which RivaTuner overwrites (in TR2013 the
  overlay never drew while it ran). The overlay uses vtable hooks unless `overlay.hooks` is `"inline"`.
  `Cargo.lock` pins its commit (`cargo update -p hudhook` picks up new ones; after changing the branch, `cargo fetch`).
  A local checkout at `H:\hudhook` tracks the same branch; to build against local changes there, add
  `--config "patch.crates-io.hudhook.path='H:/hudhook'"`. The patch's version must match the
  `hudhook = "..."` requirement or cargo silently ignores it and uses crates.io.
- CI: `.github/workflows/rust.yml` (build, the 32-bit overlay too, `clippy -D warnings`, and test on push/PR to
  master), `main.yml` (release on `v*` tag, after the tests: `tomb-helper.exe`, `tomb_helper_ui.dll`,
  `tomb_helper_ui32.dll`, and `tomb-helper.zip` of all three). The code isn't rustfmt formatted (lines go to 120).
  Versioning via `cargo-release`.
- `.cargo/config.toml` holds the `crt-static` rustflags (static CRT so the exe/dll run standalone).
  It must stay under `.cargo/` — cargo ignores a `config.toml` at the repo root.

## Runtime flow (`src/main.rs`)

1. `main()` hands off to `cli::run` if given any CLI args; otherwise loads config and offsets (`OffsetsSource`), prints hotkeys, then polls every 100ms with `find_process::find_process(...)`, reloading the offsets when the file changes.
2. On a match (a `find_process::Connection`), `connect()` builds a `Vec<Box<dyn Handler>>` (`create_handlers`):
   - `CutsceneTimingGeneratorHandler` **or** `CutsceneHandler` (chosen by `config.record_cutscene_timing`)
   - `OverlayHandler`
   - `PositionHandler::new_position_handler` (Lara position) and `::new_look_at_handler` (ROTTR look-at)
   - `MenuHandler` (the level select hotkey, where the version has `MenuState`)
   - Each constructor returns `None` if the game version lacks a required `AddressType`, so features
     silently disappear per-version.
3. Starts the hotkeys (`hotkeys::Hotkeys`, a thread polling the bound keys, repeating held ones but for toggles, `Action::repeats`); keys are grouped so one key can trigger several
   `Action`s, sent over an mpsc channel.
4. Every 5ms (`TICK_INTERVAL`, sleeping for what's left of it): dispatch the received `Action`s to every handler's
   `handle_action`, then call `handle_tick` on all. Exits on game process exit (`is_process_running`) or SIGINT. Errors are `eprintln!`ed (`report`), except the game
   being unreadable (`game_memory::is_unreadable`: `Unreadable`, or Windows refusing a read), which is expected during
   loading screens and exiting, so only shown with `debug_logging`. Make errors for reads that fail with `unreadable`.
5. Every 500ms it checks the offsets file (`reload_offsets`); if the running version's offsets changed the handlers are
   recreated (losing their state), and if the game no longer matches any version it disconnects.

## File index

Each module's `//!` doc says what's in it and how it works; this is where to look.

| File | What's there |
|---|---|
| `src/main.rs` | Entry: process polling, handler construction, hotkeys, the action/tick loop; hands CLI args to `cli` |
| `build.rs` | Builds the 32-bit overlay with a second cargo (its own target folder) and copies it next to the exe |
| `src/cli.rs` | The command line tools: `port`, `update-offsets`, `check` (`--fit`, `--snapshot`, `--replay`), and the rest under the hidden `debug` |
| `src/port.rs` | `port`: adding a patch or game step by step, with prompts (the other commands' steps in order) |
| `src/logging.rs` | `tomb-helper.log` next to the exe (shared with the DLL, which logs through `tracing`): the exe's `println!`/`eprintln!` are its macros, which log too; panics; lines marked `[tomb-helper pid]` or `[overlay in Game.exe pid]`; 50MB then rotated to `.1` |
| `src/config.rs` | `tomb-helper.json`: hotkeys, cutscene files, the overlay's starting settings |
| `src/action.rs` | `Action`: everything a hotkey can be bound to |
| `src/hotkeys.rs` | `KeyCode` (the keys' names in the config) and `Hotkeys`, polling the keys, repeating held ones |
| `src/handler.rs` | The `Handler` trait every feature implements |
| `offsets.json` | **Per-game-version offsets** (built into the exe), layouts and `words` |
| `src/offsets_file.rs` | The offsets file's JSON, `OffsetsSource` (the file next to the exe overrides the built in one, hot reloaded), `save` (a version's entry), `parse_hex`, `describe_chain` |
| `src/process_details.rs` | Types: `AddressType` (the offsets file's keys), `ProcessDetails`, `VersionIdentifier` |
| `src/find_process.rs` | Finding the game process and which version it is: a `Connection` |
| `src/game.rs` | `Game`: how the overlay and `check` both find things (Lara, her proxies, probes, ...) |
| `src/game_memory.rs` | Reading the game: `Handle` (its `Memory`, live, recording or snapshot, read with its version's pointer size and layout), readers, `*_at` accessors |
| `src/layout.rs` | `Layout`: where things are in the game's structures, per version, ROTTR Steam's by default; its fields by name and kind (offset, `@read_size`, `@value`) |
| `src/words.rs` | The offsets file's `words`: names and class name parts the tools go by |
| `src/rtti.rs`, `src/components.rs` | MSVC RTTI (32/64-bit), and components found by RTTI class name |
| `src/object_list.rs` | Instances: positions, names, transforms (`to_world`), model bounds |
| `src/collision.rs` | Lara's collision: hit capsules, ground probes, her proxy and attached proxies |
| `src/collision_mesh.rs` | The level's collision meshes near a point, and raycasts against them |
| `src/aim_sweep.rs` | TR2013's aim sweep (weapon rod) through her inventory to the weapon's tuning |
| `src/trigger.rs` | What an instance is (interactable, trigger, NPC), trigger categories and shapes |
| `src/trigger_scripts.rs` | What sets triggers off and what they do, from a background scan for script nodes |
| `src/overlay_handler.rs` | Injects the overlay and sends it what to draw each tick (of the collision triangles near Lara, only those it would draw, worked out in a thread of its own: doing it in the overlay took ~20ms of the game's frame); `debug overlay-data` |
| `src/inject32.rs` | Injecting the 32-bit overlay into a 32-bit game |
| `src/ui_ipc_data.rs` | The shared memory: `UiData`, `UiSettings`, double buffering (shared with the DLL) |
| `src/lib.rs` | The injected overlay (DLL): its window and drawing |
| `src/position.rs` | `Position`, with `Vector3` conversions (shared with the DLL) |
| `src/projection.rs` | Projecting onto the screen (the camera's `Projector`), fading, and the occlusion buffer; `visible_triangles`, which collision triangles the overlay draws, worked out by the exe (shared with the DLL) |
| `src/position_handler.rs` | Save/restore position, flight mode and movement |
| `src/skew.rs`, `src/game_call.rs` | The game's own noclip (skew) for flight mode (`flight_skew`), and running a game function in a thread of the game's to send its message |
| `src/class_fields.rs` | Which RTTI class each layout field is in (`FIELD_CLASSES`), the fields a class's virtual functions use, and another build's or game's layout from that (`debug class-fields`, `debug class-layout`) |
| `src/targets.rs` | What tomb-helper writes: chains built from the structures (or the offsets file's), checked before writing |
| `src/tracked_memory.rs`, `src/tracked_position.rs` | Cached values at pointer chains, for the position and cutscene handlers |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR) and time saved |
| `src/menu.rs`, `src/menu_handler.rs` | The game's menus (`UiMenuStateManager`, by name), and the level select hotkey that asks for its hidden level select |
| `src/fall_start.rs` | Finding the fall start (and her velocity) by lifting Lara a little twice (`debug find-fall-start`, run by `port`) |
| `src/flight_fields.rs` | Finding the other fields flight mode writes through from the structures and code: the proxy's collision bit and resolved centre, the camera's switch, the body list's previous link (`debug flight-fields`, run by `port`) |
| `src/camera_fields.rs` | The camera's fields by what they have to be (inverse matrices, the first camera-to-world one, fov then aspect), for `update-offsets` and `check`; `debug camera-fields` |
| `src/value_layout.rs` | A layout from the values in it: Lara's proxy's fields found by their values in another game's or build's snapshot (`debug value-layout`, used by `update-offsets`) |
| `src/cutscene_id.rs` | Finding `CutsceneId` while a cutscene plays, by the timing file's ids (`debug find-cutscene-id`, run by `port`) |
| `src/cutscene_timing_info.rs`, `src/cutscene_timing_generator_handler.rs` | The cutscene timing file, and recording it with LiveSplit |
| `src/readable_from_path.rs` | JSON from a path or URL |
| `src/check.rs` | `check`: reads what the overlay does and checks it makes sense |
| `src/snapshot.rs` | `check --snapshot`/`--replay [--accept]`: `check` without the game; where dumps are kept |
| `src/discover.rs` | The overlay's statics through RTTI (for `update-offsets`; `debug discover`) |
| `src/update_offsets.rs` | `update-offsets`: finding a build's offsets (`--exe` for a new game; derives from the last build's dump) |
| `src/fit_layout.rs` | `check --fit`: searching for layout offsets `check` passes with |
| `src/derive.rs` | Offsets for a new build by matching code references (`update-offsets --from`; `debug derive`, `trace`, `check-matching`) |
| `src/normalised.rs` | Matching references between compilers |
| `src/code_trace.rs` | Tracing how code uses a static: the pointer chains it accesses |
| `src/chains.rs` | `connect` (for the live tools), `debug chains` and `debug probe` |
| `src/watch.rs` | `debug watch` (and `watch-raw`, `disasm`): hardware breakpoints for the instructions accessing values |
| `src/watch_fields.rs` | `debug watch <group.field>`: the instructions accessing layout fields |
| `src/pe_image.rs` | `PeImage`: an exe or dump in memory layout, or dumped from a process |

## Common changes

- **After changing code that reads the game, or the offsets file:** `tomb-helper check --replay` (snapshots of each
  game in the data folder's `snapshots`, taken with `tomb-helper check --snapshot`) shows any game whose `check` report
  changed.
- **Differences between games:** put them in the `layout` (an `Option` that's `null` for what a game doesn't have),
  or detect them from the game itself (e.g. which RTTI classes its image has), rather than checking the pointer size
  or which game it is; read memory through `game_memory` and RTTI through `rtti`, which handle 32 and 64-bit.
- **New patch / game:** `tomb-helper port` (`--exe Game.exe` for a new game) does all of it with prompts: `update-offsets`
  (deriving from the newest dump of the game's last build), `check --fit` if needed, a snapshot, the cutscene id if
  unsure, the flight fields, and the fall start if unknown (a lift). Fix what's
  left of that version's `layout` by hand until `check` passes (README "Porting the overlay"). It saves to `tomb-helper-offsets.json` next to the exe, which hot reloads; copy
  the entry into `offsets.json` once it works. Use `image_size` when the version string is shared between stores. Only
  include the `AddressType`s you've found — handlers needing missing ones won't load. New fields read from the game go in
  `Layout` (with ROTTR's value as the default), not constants, and if they're in an object with RTTI, in
  `class_fields::FIELD_CLASSES`: then `update-offsets` works out where they are in a new build from how the class's code
  uses them (from the previous build, or for a new game the other games' dumps, which have to agree). Those are labelled "its class's code" in its summary; they've been right but for one in 11 checked, so
  `check` has to pass before they're trusted. `debug class-layout <from> <to>` shows how well it does between two dumps.
  How each part is found, and what was found in each build, is under "How `port` finds things" below.
- **New layout fields:** a field that isn't an offset in its structure is marked in its `group!`: `@read_size` for how
  much of the structure is read (it grows when `check --fit` moves the fields), `@value` for a record size, count, type
  number, mask or index (which never moves).
- **Reading the game:** through a `game_memory::Handle` (or the `Game` holding one), which has the version's layout and
  pointer size (`handle.layout()`, `handle.pointer_size()`, `handle.pointer_at(..)`); there's no global layout, so two
  games (or one game with two layouts, `Game::with_layout`) can be read at once. The live tools start with
  `chains::connect`, a `Connection` (pid, process handle, base address, details) whose `game()` reads it.
- **Add a new memory value:** add an `AddressType` variant, add offsets per version in `offsets.json`, read it with
  `Game` (`read_floats`, `require` and `game_memory`'s readers), or `TrackedMemory` in a handler that writes it.
- **Writing a value:** anything a handler writes goes in `targets::WRITTEN`, with a check in `targets::verify` (what it
  should look like, and the RTTI class it's in), and is found through `targets::find` just before writing, so a chain
  that's wrong for a build is never written through. Where it's a field of something tomb-helper can already find (Lara's
  instance or components, her player object, GameRender), add the field to `Layout` and build its chain in
  `targets::structural` rather than adding offsets per version: then a new build ports it for free.
- **Add a hotkey action:** add a variant to `Action` (`src/action.rs`), handle it in the relevant
  handler's `handle_action`, optionally add a default in `default_hotkeys()` (`src/config.rs`), and
  document it in `README.md`.
- **Add a new feature module:** implement `Handler` (its constructor takes the `Connection`), add `mod` in `main.rs`,
  construct it in `create_handlers()`.
- **Send more data to the overlay:** add a field to `UiData` (`src/ui_ipc_data.rs`), fill it in
  `OverlayHandler::handle_tick`, read it in `TombHelperHud::render` (`src/lib.rs`), and update `SHARED_SIZE` to the
  size the build error gives. Both binaries must be rebuilt together.

## How `port` finds things, and what it found in each build

What the porting tools go by, and what's been found with them; the structures themselves are in the game memory notes
below.

- SOTTR's `CutsceneId` is used through a pointer to a record (`[r14+4]`), so its chain can't be derived for sure; it's
  held back as unverified, and `port` finds it while a cutscene plays: the cutscene's
  length picks its timing file entry, whose ids are the same in every build (`cutscene_id`). Deriving 234.1 and 492
  from each other gets everything else right (`player.instance` comes from `XPosition`'s chain).
  The games share their gameplay data, so with Lara standing somewhere ordinary her proxy's fields hold the same values
  in every game: `value_layout` finds them by value in the newest snapshot of the previous build (or for a new game, of
  each other game), in order. Every pair of the 5 snapshots (ROTTR, SOTTR 492/234.1, TR2013 Steam/748, 32 and 64-bit)
  got all 9 known proxy fields right. `update-offsets` applies it after the class code; like it, it only moves fields
  the layout has (it predicts ROTTR's `resolved_centre` at +0x70, unconfirmed). Camera and player fields hold
  values that depend on where she and the camera are, so they aren't found this way. The camera's are found by what
  they have to be instead (`camera_fields`, no other build needed): in GameRender, `view` is the one world-to-view
  matrix (the inverse of a camera-to-world one; those all put the camera near Lara), `sin` the first camera-to-world
  one, and the lens's fov comes just before its aspect, after the near and far planes. Checked live in ROTTR, SOTTR
  492 and TR2013 Steam, and from ROTTR's and SOTTR's layouts it finds all of TR2013's. Fields that pass are kept (ROTTR's
  lens has two fov copies, and the first can be another pass's). `player.instance` comes from `XPosition`'s chain; 8
  fields of the player point at Lara's instance in ROTTR and SOTTR, the right one the 4th in both. The fall start is
  found by lifting her 150 then 300 up (`fall_start`): the fields of her player object (RTTI `Player`, found from her
  instance a pointer or two in) that follow both lifts are it and its copy. In TR2013 Steam that found the player at
  [[instance + 0xA8] + 0x14] (as the aim sweep knew) and the fall start at 0xF0C and 0xF18, 0xC apart as SOTTR's are;
  TR2013's position chain doesn't go through the player, so `targets` finds it by its class from her instance, else
  the layout's way: in ROTTR (and SOTTR) nothing within three pointers of her instance leads to the player (her user
  data doesn't), only the player to her (`player.instance`, through `XPosition`), so both stay, and `fall_start` takes
  whichever gets there. Flown down in TR2013 and lived.
- TR2013 Steam has every flight value SOTTR has but the body's gaps and previous centre, found live:
  velocity at her instance +0xA4 -> `cdc::BallisticComponent` +0x10 (fell by ~1 a frame during a lift, 0 on landing;
  it's in the table in her instance, as ROTTR's and SOTTR's are at +0xF0, +0x20 into theirs, the same layout 0x10 on);
  `PlayerProxy` flags +0x10 (0xB3 standing, 0x93 in the air: SOTTR's 0x4B3/0x1493, same low byte), whose bit 0x1 gates
  the proxy update (+0x1705E0) copying the collision result ([proxy+0x108]+0x80) into +0x60, the resolved centre (which
  lagged +0x20 a frame during the lift); camera collision at [[[GameRender]+0x5F0 (`CameraComponent`)]+0xA2C
  (`ExplorationCameraController`)]+0x914, a struct at +0x910 (vtable, enabled byte, 15, 15, 30, -> a `cdc::GeomSphere`
  query), whose byte is read every frame by three tests that skip collision when it's 0 (+0x511572, +0x511DCC,
  +0x520BF7), like SOTTR's three passes. targets builds the proxy's fields from `PlayerProxy` where there's no
  component table. Not flown with them yet.
- It has a physics body after all (`proxy.body` +0x10C, which the notes above once said it hadn't): body +0x0 -> shape,
  +0x38 next (423 in the list; +0x3C the other way), +0x40 -> rigid body (also proxy +0x108: inertia, mass 100 at +0x3C,
  +0x80 position = the proxy's centre), +0x44 parent (the combat proxy's body has hers, 20 along the list), +0x48 -> its
  proxy +0xF8. Shape as ROTTR's (+0x0 centre, +0x20 rotation, +0x44 radius/length 25/60) but type at +0x70 and touching
  at +0x71 (0x2F). Unlike SOTTR's it isn't swept towards her: the proxy's set-transform (+0x175800, with no test of the
  collision bit) works out her centre from her feet and places the body there (+0x1EF960 -> +0x1F2D70, written at
  +0x1333AB), so there are no gaps to clear and it can't be left behind a wall.
- The combat proxy's body is attached to hers like ROTTR's camera and weapon, so the overlay finds it through her body
  list as in ROTTR, and its proxy through the body's owner (+0x48 -> proxy +0xF8, `body.owner`/`proxy.body_owner`,
  checked by the proxy pointing back at the body), whose flags say whether it collides (checked aiming at a wall). So
  there's no `CombatProxy` offset any more (nor the layout's `proxy.attached`). v1.01.748's body is at +0x10C too, found from its code (it loads +0x108/+0x10C right after testing its
  collision bit, flags +0x8, as Steam's does at +0x10) and checked live, with Steam's body layout.
- v1.01.748 has all of Steam's flight values now, found the same ways and checked live: its record's component table
  starts at +0xA0 (Steam +0xA4; same slots, so velocity is ballistic +0x10, whose start matches Steam's value for
  value), its code tests the collision bit (+0x108AD7) and copies the collision result into +0x60 as Steam's does, the
  camera collision is Steam's path ([GameRender] +0x5F0, +0xA2C, +0x914; tested at +0x4D4AE3), and lifting her found
  the player at [[instance + 0xA4] + 0xC] and the fall start at +0xF0C/+0xF18, Steam's offsets.
- TR2013 Epic (v1.01.0.0) is Steam's build with its statics moved (mostly by 0x1930) and Steam's layout: `port` found
  it all from Steam's dump (`update-offsets` takes ~10 minutes on it), but for `InventoryPool`, unverified between
  0x1F6E160 and +4, which its handle entries (record, -1, id, 0) and first record (-> Lara's instance) settled.
  Its fall start's two copies differed (25 and 88 below her) where Steam's always matched; the fields round them are
  Steam's. Flown and the overlay checked in game.
- ROTTR Steam has all of SOTTR's flight values, found live the same ways (in its entry, not the defaults, as SOTTR's
  resolved centre differs): the fall start at player +0x1074 and +0x1080 (lifting her, the player through her position
  chain, 0xC apart as in the others); flags +0x18 (0x4B3 standing, as SOTTR's), whose bit 0x1 has its proxy
  code (+0x1DB4E6) copy her centre (+0x30) into +0x70, the resolved centre; velocity +0x20 in its ballistic component
  (TR2013's layout 0x10 on); camera collision [[[GameRender] +0x680 (`CameraComponent`)] +0xAB0
  (`ExplorationCameraController`, pointed at directly as in TR2013, not through SOTTR's camera stack)] +0xBB8, a byte
  before its -> `cdc::GeomSphere` (+0xBC8) that three compares in its code test (+0x7DF18C, +0x7DF2F0, +0x7DF847); and
  its body and shape have SOTTR's gaps (body +0x0, shape +0x10, zero standing) and previous centre (shape +0xA0), so it
  gets SOTTR's after-flight fix-up (whether it's needed isn't known). ROTTR Epic 1027 has the same layout and was
  ported by `port` from Steam's dump, but for its position chain, which pointed at her instance directly so the player
  (and fall start) couldn't be found; it goes through the player as Steam's does. Flown and the overlay checked in game.
- `port` finds all of that itself now, the same way in every game (`flight_fields`, read only, every port; the lift
  only where the fall start isn't known): the resolved centre is the proxy's field that's a store through the same
  register within a few instructions of a byte test of a single bit at `proxy.flags`, and holds her centre exactly
  while she stands (that bit is the collision bit); the camera switch is a set byte in the 0x40 before the controller's
  `cdc::GeomSphere` (inline or pointed to) that the most byte compares in the code use; the previous link is the offset
  near `body.next` that leads back from each body's next. Velocity is found during the lift, the vector in her ballistic
  component whose vertical part is under -0.5 while she falls and 0 once she's landed. With ROTTR's fields removed it
  found all four as they'd been found by hand, and so it did in TR2013 Steam and 748 (whose camera switch has only one
  compare with its full displacement; the other readers go through the struct). In SOTTR 492 and 234.1 it agreed but
  for the camera, which it found directly (above), and confirmed the previous link +0x40.
- The body list is walked whole from its start (`body.previous`, checked as it's followed) about every 5s, or when a
  remembered body stops being attached: ~5us a read, so a whole walk is ~5ms, too slow for every tick. TR2013: 437
  bodies, +0x3C previous, starting 14 before hers; ROTTR: 733, +0x38 previous (707/707 checked), 25 before hers;
  SOTTR: +0x40 (found by `flight_fields`).
- Components are learnt in both an instance's tables, the one it points at and the one in it (`instance.inline_components`;
  ROTTR and SOTTR have both, from +0xE0: transform, hierarchy, ballistic +0xF0, user data, game flags; TR2013 only the
  one in it), each class's slot remembered as an index or an offset (`components::Slot`). So velocity is found the same
  way in every game, and the layout's `instance.ballistic` is gone.
- Still not found the same way: Lara (TR2013 has no `PlayerComponent`; her proxy points
  back at her instance, +0x18), hit capsules (there's a `CollideComponent`, but no fixed route to it from the record was
  found: one that held for her held for 20 of 141 instances) and model bounds (no `DrawableComponent`; her record +0x94
  leads to a `RenderModelInstance`).

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

### Flight mode (SOTTR Steam 492)
- Holding her by writing her position isn't enough: the game moves her every frame, so `PositionHandler` writes from a
  thread every 1ms. Found with Python polling and `watch-raw` while she was held in the air and flown through walls.
- Velocity: [player+0x35C0 (her instance)]+0xF0 -> `cdc::BallisticComponent` +0x20, integrated (with gravity at +0x30) by
  the player's movement (+0x9A22E0), which adds the result to her position (+0x99E980, a delta per kind summed at
  player+0x490). player+0x710 is only an average of past velocities (ring buffer at +0x700). Zeroing it stops the drag.
- Fall damage goes by player+0x1064 (and a copy at +0x1070), the height she last left the ground at: flown 15m down
  with it untouched, she died on landing; moved with her, she lived.
- Her proxy (component slot 39): +0x18 flags (0x4B3 standing, 0x1493 in the air), bit 0x1 is its collision; +0x70 its
  centre (follows her), +0xC0 where collision put it, which the game moves her towards (up to ~30-100 a frame), so it
  pulled her back through a wall she'd flown through, and up out of the ground. Flight clears bit 0x1 and copies +0x70
  to +0xC0. With bit 0x1 clear her physics body (proxy+0x1A8 -> body, +0x20 -> shape: +0x0 centre, +0xA0 a copy) stays
  where it was, and once it's set again the body only moves towards her (body +0x0 and shape +0x10 are how far it has
  left, zero normally), stopping at walls: it stayed ~1000 away, and she could jump through walls until the game was
  restarted. Setting the shape's centres to the proxy's and zeroing those two after the flight puts it back on her.
- The camera: `GameRender`+0x450 -> Lara's `CameraComponent`, +0xD40 -> its `ExplorationCameraController` (as found
  first, through +0xF08 -> the active camera stack entry, +0x20, but that isn't always there: once it held floats, so
  `flight_fields` takes the direct pointer, as in ROTTR and TR2013), whose collision struct at +0xDF0 (`cdc::GeomSphere` at +0x20, radius 10)
  has an enabled byte at +0x8 that every collision pass checks (+0x8AF920, +0x8AFF30, +0x8B0D80): zeroed, the camera
  goes through walls. Below ground it also follows her proxy's resolved centre, so that needs the proxy's collision off.
- Still left: a small jitter in the air (probably her falling animation).
- Flight mode (`position_handler`) doesn't write any of the collision fields for now (2026-10-28): her collision was
  still broken after flights (she could jump through walls), so it holds only her position, velocity and fall start.
  `targets` still finds the proxy flags, resolved centre, body fix-up and camera switch for when it's tried again.
- The game has its own noclip, the cdc "skew cheat" (tried in SOTTR Steam 492, 2026-10-28): Lara's player message
  handler (492 mod+0x99F2C5, 234.1 mod+0xD9C1C05) turns it on for message 0xA09458BC with non-null data and off with
  null, switching off her gravity and collision and putting her player state machine in a skew state (player+0x7C bit
  0x2). Messages are sent with mod+0x8E6D70 (the message system [mod+0x36AAF30], instance, hash, data), which takes the
  system's lock and calls the instance's handler there and then. Sent from a thread of tomb-helper's own
  (CreateRemoteThread on a stub) it worked: the camera zoomed out a little, WASD only turned her, and a single write
  of her position held her there, in the air or under the ground. Turned off, she fell, landed and walls blocked her
  again. Its movement was message 0x39819B68 (a vector, scaled and added to player+0x6B0, then moved by +0x99E980),
  sent by the debug controls, which are compiled out. Flight mode uses it with `flight_skew` in the config (on by
  default since 2026-10-30; it runs code in the game): skew on as a flight starts, off as it ends (`SendMessage`, `MessageSystem`
  and `player.skew` in 492 Steam's entry); flown through walls and the ground, she landed and collided as normal.
  Every other build with a dump has them now (2026-10-30), found the same way: the handler's "Skew Cheat Enabled!"
  string, or its compare with 0xA09458BC, leads to its on path, which sends 0x4720A8C2 with the send-message function
  and its message system (`mov rcx, [static]` / `mov ecx, [static]` just before; ~75% of the function's callers load
  that static) and tests or sets the skew bit on the player. ROTTR, SOTTR Epic 492 and 234.1 (whose function is a
  Denuvo jmp thunk) are 492 Steam's code with player+0x7C bit 0x2. TR2013 (Steam, Epic, 748) is 32-bit:
  the function is `__thiscall` (system in ecx; instance, hash, data pushed; `ret 0xC`), its instance the 0xD0 record
  her position chain reaches, and the bit is player+0x60 0x8 (`player.skew_mask`), clearing bit 0x1 as SOTTR's does.
  Sent from a thread of its own in TR2013 Steam it set and cleared that bit; not yet flown with it in TR2013 or ROTTR.
- These are all fields of things found without offsets (her instance, components and player object through `XPosition`'s
  chain, GameRender through `CameraView`'s), so `targets::structural` builds them from 492's layout (`instance.ballistic`,
  `player`, `ballistic`, `proxy.flags`/`resolved_centre`, `body.gap`, `physics_shape`, `camera.collision`); a test checks
  the layout gives exactly the chains above. Other versions only need those layout fields to get flight mode.

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
    -> `cdc::SceneEntity`, +0xA4..+0xD0 a component table in the record itself (`instance.inline_components`: ballistic,
    user data, soft body, game flags, `GameInstanceData`; fixed slots, right for every instance that has them, learnt by
    class like ROTTR's, without the back pointer check: only some point back, at +0x10). Lara was record 2
    (mod+0x1D24AB0), which the position chains reach. In the offsets file (`InstanceArray` [0x1D24910], layout: header
    +0xC0 (link indices, set up to the end), in_use +0x40 (-> object type, whose +0x4C -> name, e.g. "laracroft"), id
    +0x48 (not quite unique), no placement/components/transform). +0x64 -> component data: +0x8 count, 0xC byte
    entries from +0x14 (hash, -> name, -> data), e.g. InteractComponent, LootComponent, RagdollComponent,
    InventoryMemberComponent (Lara's gear), which `trigger::read_kinds` goes by when there's no component table.
  - Lara's movement proxy is a statically allocated `cdc::ProxyComponent` at mod+0x1113830 (`PlayerProxy`, layout
    `proxy`): +0x10 u32 flags (bit 0x20 clears while aiming, as in ROTTR), +0x18 -> its instance, +0x20 centre, +0x48
    radius/length (25, 60; aiming 25, 40), +0x50 offset (0, -5, 85; aiming (0, -10, 90)), +0x70 tilt and +0x80 facing
    quaternions, +0xA0/+0xB0/+0xD0/+0xE0 the shape it's changing to, +0xC0 her feet (feet + facing * offset = centre,
    checked), +0xF8 -> what it's attached to (her instance), +0x10C -> its physics body (see below). Contacts: u32 count at
    +0x120 (the records keep their old values when it's 0, which is why they looked stale), 0x50 byte records from +0x130
    (normal + distance, then the point at +0x10). Aiming at a wall she faced, every live contact was on the combat
    proxy (`collision::read_proxy`'s `standalone`), none on hers.
  - The combat proxy is another `cdc::ProxyComponent` in the same static pool (0x7B0 apart; it was at mod+0x11175B0 one
    session and mod+0x1113FE0 the next, the slot zeroed), pointed to by Lara's proxy +0x788 (its own +0x788 is
    null, so maybe a list of attached proxies; found through her body now, see below), belonging to the "combatproxy" instance and
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
  by sampling while aiming: the combat proxy's bit 0x2 is set and Lara's bit 0x20 clears while she aims. 
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

### Menus and the level select (SOTTR 234.1)
- `UiMenuStateManager` (RTTI; SOTTR 234.1's at [mod+0x13D4E58], a `UiSingleton`) is a state machine of the menus:
  +0x330 + id*8 the state objects (each with its id at +0x60), `menu.names` the names by id ("dtp::MENU_STATE_*",
  0x40 each, `static_cast<dtp::MENU_STATE>(counter)` for unused ids, 0x7D of them), `menu.current` the menu showing,
  `menu.requested` (-1 for none, -3 closes the menus) and +0x7A0 the previous. The code that switches (mod+0xDD25DF0,
  every frame) leaves the current menu (its vtable +0x68, which can refuse) and enters the requested one (+0x50). Found
  by watching the pause state's methods with `watch-raw --exec` while pausing.
- The level select (`UiMenuStateLevelSelect`, id 0x13; ROTTR 0x19) is built with the rest. Requested from the main
  menu it works: categories -> levels -> 365 start points (data at [mod+0x3525268] +0x2668: 0x20 byte entries, +0x8
  name, +0x10 enabled byte, +0x14 count, +0x18 children; leaves +0x18 the unit path), and choosing one loads it. From
  the pause menu its page doesn't show. The main menu's "SELECT LEVEL" button (id 10 in its table at state
  +0x330/+0x338, shown byte +5) is always hidden by its code. The disabled "Multiplayer" category (gym levels, `Gym\Coop\gym_coop`)
  says "content is currently installing": not shipped. `MENU_STATE_MATERIAL_TEST` (0x7B) is an empty stub.
- Lara's message handler (mod+0xD9C1540) still turns skew (the cdc fly cheat) on for message hash 0xA09458BC with
  data, off without ("Skew Cheat Enabled!", player+0x7C bit 2), but nothing sends it (the `INPUTACTION_Debug_*`
  handling is compiled out).
- ROTTR Steam's (the defaults: 0xA28, 0xA2C, names 0xAC8, level select 0x19, also a `REPLAY_LEVEL_SELECT` 0x6C) were
  found in the switching code and checked live, with the static at mod+0x2E1E758; ROTTR Epic 1027's are the same,
  checked live, with the static at mod+0x2CAB540. SOTTR 492's `menu` fields (0x7A8, 0x7AC, names 0x840) were found in
  the switching code in its dumps and checked live in both, with the static at mod+0x146BF38 (Steam) and mod+0x1450E18
  (Epic).
- TR2013 Steam has the manager too ([mod+0x20CF83C]; 0x59 menus, unused ids named "counter"), but with i16 ids
  (`menu.id_size` 2): current +0x1AC, requested +0x1AE, names +0x1F4, from its switching code (mod+0x64A870, which
  treats a request of 0xFE specially). Its `LEVEL_SELECT` (0x1D, among the multiplayer menus, so probably the
  multiplayer map picker) shows nothing from the main menu: the menu goes, leaving the scene. So its entry has the
  layout but not `MenuState`, and the hotkey doesn't load there. Its states by class (manager +0x8, 4 bytes each):
  LEVEL_SELECT, TEST, TEST2, MODE_SELECT, CHARACTER_SELECT and the MULTIPLAYER_OLD ones are `UiMenuStateGeneric`
  placeholders. `UiMenuStateChapterSelect` (0x51) is a developer chapter select: it lists a table at
  [mod+0x20CF834] (+0x50 count, +0x4C 0x14 byte entries: name, id, unit, dtp id, runnable) holding three test chapters
  (units `tedpersistentdata`, `tedscratchpad`, `tedcampsite`, which aren't shipped; runnables `Test\test`,
  `Test\test2`, `Test\testPersistence`, which are), but requested from the main menu it doesn't show (and the main
  menu stayed current). `UiMenuStateStreamLayerDemo` (0x37) loads the unit `uiplus_streamlayerdemo` (spheres over
  the sea, no menu), and Back leaves it. `AGDebugMenuTriggerNode` is compiled out. TR2013's archives (`.tiger`, TAFS
  v3, `pc-w\`) hash names with the same CRC32 as ROTTR's.

### Other developer leftovers (SOTTR Steam 492, looked at read-only)
- God mode looks like a plain write: Lara's HealthComponent (component slot u32 at mod+0x16BB100) +0x2B0 ->
  `Invincibility_DS`, whose +0x28 (i64) is when her invincibility ends and +0x30 its duration; `SetInvincible`
  (mod+0x2840E0) stores -1 in both for good. The is-invincible test (mod+0x270CC0) and the damage code (mod+0x27D0F0)
  treat a negative end, or one after now, as invincible (unless the hit has bit 0x2 at +0xBE, probably scripted
  deaths). Lara's read 0/0 (not invincible). Not written yet.
- Free camera: the camera controller factory (mod+0x17A050, by a description's type at +0x18) makes a
  `FreeCameraController` for type 0 (1 Simple, 2 Iso, 3 MapView, 4 Modelview, 5 Anchored, 6 Spline, 7 Dummy, 13 Hmd);
  skew's on path pushes a type 1 description with mod+0x166E50(camera, settings, description), so a free camera would
  be the same call with type 0 (code run in the game). Its update integrates its own fields (+0x1A0..+0x1F4) and reads
  no input, so something would have to drive it.
- Scripts (ActionGraphs) are 540 node classes (`AG*`), each doing its work in vtable slot 1 by calling engine
  functions directly (e.g. `AGPlayerGiveLootNode`: the source instance's LootComponent and the player into
  mod+0x6ED590, then +0x6F1AA0; `AGInstanceTeleportNode`: a sequence of moves, then message 0x293EC205 to her).
  Objects talk through messages: `AGMessageSendNode` holds the hash (+0x50), target instance id (+0x28) and a 0x20
  byte payload (+0x30, mostly `04 00..`), and sends them with mod+0x8E75B0 through the same message system as skew.
  A level's scripts sent 212 distinct messages from 1362 send nodes (116 to Lara, none of them ones her player
  handler takes: her other components handle those). Their names aren't in the exe.
- Names: the exe only names its PDB (`d:\tr11\tr11_dev\...\x64_gm_nixxes\game\SOTTR.pdb`, not shipped) and exports
  library functions (Wwise, NVIDIA NGX, Square Enix's osdk). What can be named from the exe: 13,429 virtual
  functions by their RTTI class (Class::vN, of 35,604 functions that are called), 76 network RPC handlers by their
  registration strings (each `lea r9, handler; lea r8, "Class::RPC_Name"; mov edx, id; call mod+0x249430`; 6 of them,
  e.g. `NetDebugger::RPC_SetOption`, have the empty mod+0xE80780 as their handler; each handler reads its arguments
  from a bitstream with mod+0x663B0 and calls the function doing the work, e.g. `RPC_SetInvincible` ->
  `SetInvincible`), and functions that use one of ~312 "Class::Method" assert/log strings. Message hashes aren't
  CRC32, FNV-1/1a, MurmurHash2 or xxHash32 of any string in the exe. The most called functions are plumbing
  (allocator mod+0x40B670, CRT stubs, string, refcounts, locks, ActionGraph helpers +0x802AD0/+0x7FE690, bitstream
  reads +0x663B0). Feral's Linux and macOS ports don't name it either (their binary depots, checked 2026-10-29, are
  in `H:\feral-linux` and `H:\feral-macos`): no debug info, symbol table or dSYM in any of the three games, and their
  strings name only middleware source (RakNet, Quazal, Breakpad, osdk, Feral's own). The Linux builds export nothing of
  the game's (TR2013's 18k exports are Feral's GL/D3D layers, libvpx, D3DX); the macOS ones export only functions
  registered by address: ActionGraph node factories (`AG*::DTPCreate`, ROTTR also `DTPIOCount`/`DTPMemSize`, 539 in
  TR2013 and 754 in ROTTR), the AI planner's `Action_*`/`Goal_*::Birth` and event listener callbacks, nothing of
  movement, camera, proxies or menus; SOTTR's macOS build exports none. Their class names (GCC typeinfo) are the
  Windows RTTI's.
- Compiled out or inert: `Debug_SetDebugNpc` (stub), `AGDebugDrawText`/`DebugDrawVector` (format text, draw
  nothing), the render debug view modes (only in the shaders' source), `INPUTACTION_Debug_*`. The player handler's 51
  message hashes have no names in the exe (not the CRC32 of any string in it).
- Launch options (`NxAppImpl`, mod+0xFE1840): `-nixxes` (sets its +8, read through a shared getter, effect unknown;
  untried), `-benchmark`, `-nolauncher`, `-nodriverwarning`, `-benchmarkini`, `-setup`, `-config`, Epic's `-DevPort`
  /`-DevCredentials`. A QA tool's message types ("copperpot_" ping, script, teleport/skew player to marker, kill
  player, reload unit, ...) are an enum's names at mod+0x146BD50, but nothing refers to the table (no code, no
  pointers in live memory; the same in ROTTR, TR2013 and 234.1), and the game listens on no ports: the code that
  handled them is gone from every retail build.

### Working with the live game
- The user plays; ask them to switch to the game before anything that needs it running (it pauses when alt-tabbed),
  then start with a delay. Reads work fine while it's paused.
- Throwaway exploration has been done with Python (ctypes ReadProcessMemory + numpy) from the job's tmp dir: run it
  with `-I`, which also ignores `PYTHONIOENCODING`, so use `sys.stdout.reconfigure(encoding='utf-8')`.
- Heap addresses change when the game restarts, but the proxy/body/shape objects stayed put within a session.

## Gotchas

- Windows-only; non-Windows stubs in `find_process.rs` just panic.
- Dumps of each build and snapshots go in the data folder: `data_dir` in `tomb-helper.json`, else `tomb-helper-data`
  next to the exe (`config::data_dir`). Here it's `H:/tomb-helper-dumps`, set in `target/release/tomb-helper.json`.
- The investigation tools the notes above name (`chains`, `probe`, `derive`, `discover`, `trace`, `watch-raw`, `disasm`,
  `check-matching`, `overlay-data`, `dump`, `export-offsets`, `class-fields`, `class-layout`) are `tomb-helper debug <tool>`; `fit-layout` is `check
  --fit`, `snapshot`/`check-snapshots`/`accept-snapshots` are `check --snapshot`/`--replay`/`--replay --accept`, and
  `watch` is `debug watch` (values, or `group.field` names).
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
  (32-bit ones for TR2013) if changes to the overlay don't show. `debug overlay-data [--delay N]` prints what the exe
  would send without injecting; it owns the shared memory, so not while tomb-helper runs.
