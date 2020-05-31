# Tomb-helper
[![GitHub Workflow Status](https://img.shields.io/github/workflow/status/zed0/tomb-helper/Rust)](https://actions-badge.atrox.dev/zed0/tomb-helper/goto)
[![GitHub release (latest SemVer)](https://img.shields.io/github/v/release/zed0/tomb-helper?sort=semver)](https://github.com/zed0/tomb-helper/releases/latest)

A speedrun helper for the Tomb Raider reboot series

Current features:
- Save and restore positions
- Move Lara directly
- Supports Tomb Raider 2013, Rise of the Tomb Raider, and Shadow of the Tomb Raider

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
		{"key": "F8", "action": {"SkipCutscene": {}}},
		{"key": "W", "action": {"Forward": {"distance": 100.0}}},
		{"key": "S", "action": {"Backward": {"distance": 100.0}}},
		{"key": "A", "action": {"Left": {"distance": 100.0}}},
		{"key": "D", "action": {"Right": {"distance": 100.0}}},
		{"key": "Space", "action": {"Up": {"distance": 100.0}}},
		{"key": "C", "action": {"Down": {"distance": 100.0}}}
	]
}
```

## Hotkeys
The hotkeys can be customised via the `hotkeys` property.
Each entry comprises of a `key` and an `action` field.

Currently the available actions are:
- `ToggleActive`
- `StorePosition`
- `RestorePosition`
- `SkipCutscene` (very experimental, can easily crash or softlock the game when used at unexpected times)
- `Forward` (can take a distance, which defaults to `100.0`)
- `Backward` (can take a distance, which defaults to `100.0`)
- `Left` (can take a distance, which defaults to `100.0`)
- `Right` (can take a distance, which defaults to `100.0`)
- `Up` (can take a distance, which defaults to `100.0`)
- `Down` (can take a distance, which defaults to `100.0`)

The available keys are listed in the [livesplit_hotkey library documentation](https://docs.rs/livesplit-hotkey/0.5.0/livesplit_hotkey/linux/enum.KeyCode.html).
