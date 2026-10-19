# Tomb-helper
[![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/zed0/tomb-helper/rust.yml?branch=master)](https://actions-badge.atrox.dev/zed0/tomb-helper/goto)
[![GitHub release (latest SemVer)](https://img.shields.io/github/v/release/zed0/tomb-helper?sort=semver)](https://github.com/zed0/tomb-helper/releases/latest)

A speedrun helper for the Tomb Raider reboot series

Current features:
- Save and restore positions
- Move Lara directly
- Skip cutscenes and record how much time was saved (in Shadow)
- Supports Tomb Raider 2013, Rise of the Tomb Raider, and Shadow of the Tomb Raider
- Connects and disconnects from games as they are started and stopped: useful in case of crashes!

# Usage

- Download the [latest version](https://github.com/zed0/tomb-helper/releases/latest) of tomb-helper.exe
- Open a `cmd` prompt in the directory it was downloaded to (you can type `cmd<enter>` in the Windows Explorer address bar)
- Run `tomb-helper.exe`

# Configuration

Tomb-helper can be configured by a file named `tomb-helper.json` adjacent to the `tomb-helper.exe` file.

The default configuration is:
```json
{
	"hotkeys": [
		{"key": "F5", "action": {"ToggleActive": {}}},
		{"key": "F6", "action": {"StorePosition": {}}},
		{"key": "F7", "action": {"RestorePosition": {}}},
		{"key": "F8", "action": {"ResetSkipCutsceneTracker": {}}},
		{"key": "F9", "action": {"ToggleUI": {}}},
		{"key": "F10", "action": {"ToggleCollision": {}}},
		{"key": "F11", "action": {"ToggleSettingsWindow": {}}},
		{"key": "Space", "action": {"SkipCutscene": {}}},
		{"key": "W", "action": {"Forward": {"distance": 100.0}}},
		{"key": "S", "action": {"Backward": {"distance": 100.0}}},
		{"key": "A", "action": {"Left": {"distance": 100.0}}},
		{"key": "D", "action": {"Right": {"distance": 100.0}}},
		{"key": "Space", "action": {"Up": {"distance": 100.0}}},
		{"key": "C", "action": {"Down": {"distance": 100.0}}}
	],
	"force_version": null,
	"cutscene_blacklist_file": "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/Blacklist.json",
	"cutscene_timing_file": "https://gist.githubusercontent.com/zed0/762a5790501af95189344834bc210616/raw/tomb-helper-timing-info.json",
	"record_cutscene_timing": {"Off": {}},
	"overlay": {"object_filters": [], "interactables": true, "npcs": true, "triggers": false, "collision": false, "object_radius": 5000.0, "object_box_size": 50.0, "lara_capsules": false, "lara_probes": true, "lara_proxy": true, "console": false}
}
```

## Hotkeys

The hotkeys can be customised via the `hotkeys` property.
Each entry comprises of a `key` and an `action` field.

Currently the available actions are:
- `ToggleActive`
- `StorePosition`
- `RestorePosition`
- `SkipCutscene`
- `ResetSkipCutsceneTracker` (reset the total amount of time of cutscenes skipped, prints out the previous total, suggest binding this to the same key you use to reset livesplit)
- `ToggleUI` (inject or remove the in-game overlay; experimental, DirectX 12 only)
- `ToggleLaraCollision`, `ToggleCoarseCollision`, `ToggleDetailedCollision` (show or hide Lara's collision, the level's collision she collides with, and the rest of it, in the overlay), and `ToggleCollision` for all of them at once
- `ToggleSettingsWindow` (show or hide the overlay's window, leaving what it draws in the world; F11 by default, and `settings_window` in the overlay config says whether it starts shown)
- `ToggleInteractables`, `ToggleNpcs`, `ToggleTriggers` (show or hide the boxes round interactable objects and characters, and the trigger volumes, in the overlay)
- `Forward` (can take a distance, which defaults to `100.0`)
- `Backward` (can take a distance, which defaults to `100.0`)
- `Left` (can take a distance, which defaults to `100.0`)
- `Right` (can take a distance, which defaults to `100.0`)
- `Up` (can take a distance, which defaults to `100.0`)
- `Down` (can take a distance, which defaults to `100.0`)

The available keys are listed in the [livesplit_hotkey library documentation](https://docs.rs/livesplit-hotkey/0.5.0/livesplit_hotkey/linux/enum.KeyCode.html).

## Offsets file

The memory offsets for each supported game version are built into tomb-helper (from [`offsets.json`](./offsets.json)). To change them or add a new version without rebuilding, run

```
tomb-helper export-offsets
```

which writes them to `tomb-helper-offsets.json` next to `tomb-helper.exe`. While that file exists tomb-helper uses it instead, and reloads it whenever it changes, even while connected to a game, so you can edit offsets and test them straight away. If the file has a mistake in it the error is printed and the previous offsets are kept until it's fixed. Delete the file to go back to the built in offsets.

Each version looks like:

```json
{
  "game": "Shadow of the Tomb Raider",
  "executable": "SOTTR.exe",
  "version": "(Steam) v1.0 build 492.0_64",
  "image_size": 73662464,
  "bits": 64,
  "offsets": {
    "XPosition": ["0x14B5BD8", "0x0", "0x33E8", "0x10"],
    ...
  },
  "witnesses": {
    "XPosition": ["0x62E5A9", "0x12AFCC"],
    ...
  }
}
```

- `version` is the version string in the game's memory, read using the `Version` offsets, unless `image_size` is given, in which case the size of the game's exe in memory identifies the version instead (Steam and Epic builds share version strings)
- `offsets` are pointer chains: the first is relative to the game's exe, the rest are followed as pointers. Values a version doesn't have can be left out, and features that need them won't load
- `witnesses` are optional, see [Finding offsets for a new game version](#finding-offsets-for-a-new-game-version)
- `unverified` is ignored, and is where the tools put offsets they couldn't confirm
- `layout` is optional: where things are in the game's structures, for the overlay, where they differ from Rise of the Tomb Raider (Steam)'s. E.g. `"layout": {"instance": {"position": "0x20"}, "collide": {"capsule_radius": "0x30"}}`. The groups and their fields are in `src/layout.rs`, with what each is. See [Porting the overlay](#porting-the-overlay)

## Overlay

The overlay (`ToggleUI`) draws a box around game objects near Lara, labelled with the object's name. Its window (top right; `ToggleSettingsWindow`, F11 by default, hides and shows it) has Lara's X/Y/Z position, the position tools (flight mode, and saving and restoring her position, as with the `ToggleActive`/`StorePosition`/`RestorePosition` hotkeys, with the saved position) and the overlay's settings, each in a section that can be collapsed.
Drawing in the world needs the `CameraView` and `CameraLens` offsets, objects `InstanceArray` (and `PlayerProbes` for Lara's ground probes, `CollisionScene` for the level's collision), which only Rise of the Tomb Raider (Steam) and Shadow of the Tomb Raider 234.1 have so far; see [Porting the overlay](#porting-the-overlay). Without them only the window is shown. Tomb Raider 2013 (Steam) has the camera and objects: boxes round interactables (from the names of their components, e.g. `InteractComponent`), animals and characters (ones with a ragdoll), and anything matching `object_filters`, all the same size (no model bounds yet), but no triggers, collision or Lara's capsules yet.
Tomb Raider 2013 is 32-bit and DirectX 11, so it needs a 32-bit build of the overlay: `cargo build --release --lib --target i686-pc-windows-msvc` (after `rustup target add i686-pc-windows-msvc`), then copy `target\i686-pc-windows-msvc
elease	omb_helper_ui.dll` next to `tomb-helper.exe` as `tomb_helper_ui32.dll`.
- `interactables`: draw a box (teal) round each thing Lara can interact with, labelled with its name: campfires, pickups, documents, relics, caches, crates, tether points, throwables, plants and other things to gather crafting resources from, ... On by default
- `npcs`: draw a box (red) round each character and animal (enemies, friendly characters, hares, squirrels, birds, ...), labelled with what it is (e.g. `rem_smg` or `trx_mountain_hare`). The box is round the capsules the game uses for hitting them, so it follows their pose. On by default
- `triggers`: draw the trigger volumes, labelled with their names: boxes, spheres, planes and the polygon volumes the game streams the level with. Off by default
- `trigger_categories`: which kinds of trigger to draw, going by their names: `checkpoints` (green), `death` (red), `streaming` (blue), `combat` (orange, including spawns), `unknown` (pink, any whose name doesn't say, which is mostly the level scripts' unnamed triggers), `camera` (grey, including look-at spheres), `effects` (purple, sounds and effects), `hints` (yellow, tutorials and notifications) and `objects` (brown, triggers that are part of bushes, pickups, campfires, ...). The first five by default, and each can be turned on and off in the overlay's window
- `trigger_opacity`: how opaque trigger volumes are filled in, 0 to 1 (0 draws just their outlines; also a slider in the overlay's window), 0.15 by default
  The outline of a trigger turns white while Lara's in it (her feet or her middle; planes never do)
- `triggers_by_action`: choose which triggers to draw by what their scripts do (`trigger_actions`, the default) rather than by name (`trigger_categories`); the overlay's window switches between the two
- `trigger_actions`: which triggers to draw by what their scripts do, from the words below, plus `instances` (they act on a particular instance), `something else` (they have a script, but nothing it does is known), `no script` and `unchecked` (not looked for yet; scripts are found in the background, which takes a few seconds). All by default, and each can be turned on and off in the overlay's window. A trigger is drawn if any of what it does is on. Shown this way, each is coloured by the first of what it does that's on, in the order above (the checkboxes show the colours) The label says what sets each one off (from the scripts too: Lara, an instance, other scripts or no script), e.g. `triggervolume (Lara)`, and underneath what the script it sets off does, when that's known: words for what it leads to (`checkpoint`, `objective`, `cutscene`, `moveset` (e.g. walking in villages), `camera`, `streaming`, `spawn`, `dialogue`, `tutorial`, `look at`, `NPC`, `lighting`, `audio`, ...), and the names of the instances it acts on or sends messages to (e.g. `npc_roaming_musician_guitar`, the place a villager plays music at). The scripts have no names, so this is all there is to go on, and links into a subgraph aren't followed, so some only have a script
- `object_filters`: objects whose name contains any of these (case insensitive) also get a box, `[""]` shows everything. None by default
- Interactables, characters and triggers can also be turned on and off with checkboxes in the overlay's window, or the `ToggleInteractables`/`ToggleNpcs`/`ToggleTriggers` hotkeys (not bound by default)
- `object_radius`: only objects and triggers this close to Lara are shown (the closest 256 of each are drawn)
- `object_box_size`: objects with a model get a green box showing its bounds, the rest (triggers, markers, ...) get a yellow box this size (half width)
- `lara_capsules`: also draw Lara's hit capsules (in magenta), which follow her pose. Off by default as it's slow
- `lara_probes`: also draw the ground probes the game casts for her feet and body (cyan lines, with a cross where they hit)
- `collision_meshes`: draw the level's collision that Lara collides with (the ground, walls, rocks, buildings, invisible walls and anything else fixed in place) near her: floors in green, walls in orange and surfaces facing down in blue. Physics props (barrels, buckets, breakables, ...) aren't included. On by default
- `detailed_collision_meshes`: also draw the level's collision for other kinds of collision ("Other collision" in the overlay's window), in purple where it isn't also drawn as hers. Each mesh's filter has a bit per kind of collision that uses it: 0x2 is Lara's movement, the rest aren't known yet (projectiles, the camera, NPCs, ...). The ground is usually one mesh with nearly every bit set, while many objects also have a detailed mesh without Lara's bit (e.g. one for each log of a pile). Off by default
- `collision_mesh_other_kinds`: which bits "Other collision" draws, as a number (all but 0x2 by default, 65533). The overlay's window has a checkbox for each bit the level uses, with how many meshes have it

These, and Lara's own collision (`lara_capsules`/`lara_probes`/`lara_proxy`), can be turned on and off while playing with the checkboxes under "Collision meshes" in the overlay's window, or the `ToggleLaraCollision`/`ToggleCoarseCollision`/`ToggleDetailedCollision` hotkey actions (not bound by default). "Collision meshes" itself turns all of them on and off at once: it's off to start with (`collision` in the config) and `ToggleCollision` (F10 by default) toggles it. The settings above are what they start as. The window also has a slider for how opaque the level's collision and Lara's collision are drawn, a checkbox to draw it fainter the further it is from Lara (fading out by the edge of the radius, faster the higher the fade strength), and one to hide the triangles behind other collision triangles (the game's own scenery doesn't hide them, and triangles smaller than about 4 pixels across can go missing). Their starting values are `collision_opacity` (default 0.25), `collision_mesh_fade` (default on), `collision_mesh_fade_strength` (default 2) and `collision_mesh_cull_hidden` (default on). "Round where Lara's aiming, while she aims" (`collision_mesh_follow_aim`, default off) draws the collision round where the camera's pointing (the reticle) instead of round her while she aims, marked with a white cross: the first collision triangle along the camera's line of sight within 200m, of the kinds being drawn.
- `collision_mesh_radius`, `detailed_collision_mesh_radius`: how far from Lara's feet the collision she collides with and the detailed collision are drawn (default 1500 each, i.e. 15m; they can also be changed with sliders in the overlay's window). The closest 163840 triangles are drawn, so this gets slow with a large radius
- `console`: open a console window in the game showing the overlay's logs, for debugging it
- `lara_proxy`: also draw the capsule the game moves Lara with, i.e. what she collides with walls with (white, or yellow while it's pushing her off something). When it changes shape (e.g. when she starts aiming) the shape it's changing to is drawn in red. While she aims, the proxies attached to Lara's also collide, and are drawn the same way: the camera, which moves behind her, and her weapon (a thin rod), which is what holds her further from walls in front of her

## Forcing a version

There is an experimental feature to force tomb-helper to use the memory offsets for a specific version of the game. This can be done by specifying the `force_version` key with the version you want. These are chosen from the [offsets file](#offsets-file) and must match the version in the config file **exactly**.

For example
```json
{
	"force_version": "v1.01.748.0"
}
```
would force tomb-helper to use the offsets for v1.01.748.0 even if the detected version of the game was v1.01.743.0.

## Cutscene blacklist file

The cutscene blacklist is the list of timings that are used to configure the time until cutscenes can be skipped.

The default value is [the list](https://gist.github.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b) that is used by both this tool and @Atorizil's [SOTTR Cutscene Skipper](https://github.com/Atorizil/SOTTR-Cutscene-Skipper).
If you want to use a different file (e.g. to try out some new timings) you can use a pastebin or github gist. Local files may be supported at some point.

## Cutscene timing file

Similar to the cutscene blacklist file the cutscene timing file is used to calculate how much time has been saved by skipping each cutscene.
The default value is [the list](https://gist.github.com/zed0/762a5790501af95189344834bc210616) recorded by zed0, which should be sufficient for any% glitched or glitchless runs, but does not contain all the cutscenes in 100%.

### Generating a new cutscene timing file

Generating a cutscene timing file can be done using tomb-helper:

- Install the [Livesplit Server extension](https://github.com/LiveSplit/LiveSplit.Server)
- Add a Livesplit Server component to your Livesplit layout
- Ensure the game's autosplitter is enabled in Livesplit
- Ensure Livesplit is set to compare against Game Time
- Start the Livesplit Server via right clicking on Livesplit -> Control -> Start Server
- Create a tomb-helper configuration file if you don't already have one (see above)
- Change the `"record_cutscene_timing"` config entry to `{"On": {}}`
	- Optionally change the config depending on where you want the timing file output and your Livesplit Server port: `{"On": {"timing_file": "./new_cutscene_timing_file.json", "livesplit_port": 16834}}`
- Launch the game
- Launch tomb-helper
	- Check it says "Loading cutscene timing generation handler..." during the start up
- Play through the entire game, making sure to watch all the cutscenes and press E repeatedly to make sure skip prompts show up
	- Skipping cutscenes via tomb-helper will be disabled while generating the timing file
	- The recording file is saved after each cutscene, if your game crashes or you want to record in multiple sessions simply continue at a later date with the same recording file
- Save the newly created file somewhere (upload it if you want others to be able to use it)
- Change your `"cutscene_timing_file"` configuration option to the location you saved the created file
- Change your `"record_cutscene_timing"` back to `{"Off": {}}`

# Finding offsets for a new game version

After a patch, launch the game, load into a level, and run:

```
tomb-helper update-offsets
```

It works out the offsets for the running build and saves them to the [offsets file](#offsets-file) next to the exe (which tomb-helper picks up while it's running), then checks them against the game (see [Porting the overlay](#porting-the-overlay)). On its own it finds:

- the overlay's offsets (camera, instance array, collision scene) through the RTTI class names of what they point to
- Lara's position, by finding her instance (the one with a `PlayerComponent`) and the pointer chain the game's code uses to reach it
- the ground probes, by their contents, and the version string

Anything that was right for the previous build is kept as it was. The rest, like the cutscene offsets, needs the previous build to compare against: pass its exe, or a dump of it (see below), with `--from <exe or dump>`. Without it, they're kept in the entry as `unverified` (and so not used) for you to look at. `--dry-run` shows what it found without saving it.

The commands below do parts of this, and are useful for investigating when something isn't found.

tomb-helper can work out most of the memory offsets for a new version of a game from a version it already supports:

```
tomb-helper derive <from> <to> [--from-version <version>]
```

`<from>` and `<to>` are paths to the game's exe, or `running` to use the game that is currently running. Games protected by Denuvo (e.g. Shadow of the Tomb Raider v1.0 build 234.1) are encrypted on disk, so for those launch the game, wait for the main menu, and use `running`, or save a copy with `tomb-helper dump [<out.bin>]` and pass that file instead.

The version of `<from>` is detected from its image size where possible, otherwise pass `--from-version` with a version from the [offsets file](#offsets-file), e.g.

```
tomb-helper derive "C:\Games\Shadow of the Tomb Raider 492.0\SOTTR.exe" running
tomb-helper derive sottr-234.1.bin "C:\Games\Shadow of the Tomb Raider\SOTTR.exe" --from-version "v1.0 build 234.1_64"
```

It prints a version entry to add to the [offsets file](#offsets-file), with a note per offset list saying where each level came from:

- `code`: the game's code uses this offset in both versions, in matching places
- `shifted`: the code doesn't use this offset directly, but a neighbouring field moved by this much
- `copied`: no evidence, kept as it was in `<from>`

Offsets marked `UNSURE` had conflicting matches or a copied level, and are put under `unverified` for you to check. The version string needs filling in by hand. Check the result in game with `tomb-helper probe [--watch]`, which prints the current value of every offset.

Derivation works best when the offsets in `<from>` follow the same pointer chains as the game's own code. Pointer scanners tend to find arbitrary chains instead, which can't be matched up between versions. To find the chains the code uses, load into a level (and start a cutscene for the cutscene values) with a known version running, then run:

```
tomb-helper chains
```

For each value it lists the chains the game's code uses to reach the same address, and prints an entry using the best of them. Some values (e.g. most SOTTR cutscene values) are only read by code that finds the object some other way, so have no code chain and keep their current offsets.

For those, record which instructions access the value:

```
tomb-helper watch CutscenePrompt CutsceneStatus CutsceneLength --delay 15 --seconds 8
```

This attaches to the game as a debugger and puts hardware breakpoints on up to 4 values, then detaches. The game pauses when it isn't focused, so use `--delay` to give yourself time to switch back to it (and get the values in use, e.g. start a cutscene). It prints a `"witnesses"` block to add to that version's entry in the offsets file; `derive` matches those instructions in the new build to find where each field moved to (`witness` in its output).

`tomb-helper trace <exe> <static address>` prints the chains the code accesses from every reference to a static address, which helps when investigating by hand.

### Tomb Raider 2013 (32-bit)

All of the above works for TR2013 too, with a few differences:

- Its later builds (e.g. the current Steam one) were made with a newer compiler than the original release, so their code isn't alike byte for byte. When `derive` can't match a static that way, it matches by what the code around each reference does instead (the strings and imports it uses, its constants and field offsets), and says `(by what the code does)`. A match is only sure if deriving it back leads to where it started, or near it. Pointer chain levels are copied rather than matched between such builds, since a recompile doesn't change how structs are laid out, so they're always `UNSURE`: check them with `probe`.
- Lara's position isn't in a static the code uses directly in any build, and the array it's in changed between builds, so find it with `chains` with the game running, rather than with `derive`.
- `update-offsets` only finds the version string itself (the overlay is 64-bit only, so its offsets aren't looked for), derives the rest with `--from`, and prints the values to check instead of running `check`.
- The newer builds don't inline small getters (like the one for the camera), so `trace` and `chains` follow what a getter returns into the code calling it.

## Porting the overlay

Most of what the overlay reads is found through the game's RTTI class names (e.g. `InteractableComponent`, `cdc::CollisionSceneClient`), which are the same between patches and in Shadow of the Tomb Raider, so there's less to port than for the other offsets:

1. Load into a level, with Lara standing somewhere ordinary, then run `tomb-helper update-offsets` (see [Finding offsets for a new game version](#finding-offsets-for-a-new-game-version)). It finds the offsets and saves them, then runs `check` on them. A build with no layout yet (e.g. an older patch of the same game) starts with the layout of the newest build of that game that has one, which is usually all it needs. (`tomb-helper discover --save` does just the camera, instance array and collision scene.)
2. `tomb-helper check` can also be run on its own. It reads everything the way the overlay does and checks the results make sense, e.g. that instances' transforms are TransformComponents, there's a collision floor under Lara, her capsules are her size and on her. Each check that fails points at a group of the `layout`.
3. Fix the failing groups in that version's `layout` (the file is reloaded, rerun `check`) until everything passes. `tomb-helper disasm <pid> <address>` and the notes in `CLAUDE.md` on how each structure was found help with finding a field that moved.

Components are found by class too, so their slots never need porting: each type's slot is learnt from the first component table it's seen in.

