# HD2 Preset Helper

**English** | [简体中文](README.zh-CN.md)

HD2 Preset Helper is a lightweight Windows utility for saving and applying loadout presets in Helldivers 2. Each preset can include four Stratagems and an optional Booster, and can be activated with a configurable hotkey.

The program recognizes the game interface through screen capture and completes selections using standard mouse and keyboard input. It does not modify game files, inject code into the game process, read game memory, or alter network traffic.

## Download and install

1. Download the latest ZIP from [GitHub Releases](../../releases/latest).
2. Extract it to a separate writable folder.
3. Run `HD2PresetHelper.exe`.

To update, extract the new `HD2PresetHelper` folder over the existing folder.
Settings are preserved. If a release changes the preset format, the app backs
up the old preset file and asks you to recreate the presets.

The application runs in the system tray. Right-click its tray icon to change
optional behavior or select **Exit** to close it.

To uninstall, exit the application and delete the extracted
`HD2PresetHelper` folder. This also removes its configuration, presets, and
logs.

## Quick start

The default preset keys are `Ctrl+Shift+F7` through `Ctrl+Shift+F12`, mapped to
preset slots 1–6.

1. Open the loadout home screen and select all four Stratagems and, optionally,
   a Booster.
2. Hold `Ctrl+Shift` to show the preset overlay.
3. Press `F7`–`F12` and release all keys. The program then saves the current
   loadout to the corresponding preset slot.
4. To apply it later, return to the loadout home screen with all four Stratagem
   slots empty and press the same key combination.

| Loadout state | Result |
| --- | --- |
| All four Stratagem slots filled | Save or overwrite the preset |
| All four Stratagem slots empty | Apply the preset |
| Partially filled | Reject the action without changing the loadout |

The Booster is optional and is saved together with the four Stratagems.

## Compatibility

Windows 10 version 1903 or later and Windows 11 are supported. The app has been
tested from 720p to 2160p in Windowed, Borderless Window, and Fullscreen modes,
including several non-16:9 resolutions and Windows HDR.

For Borderless Window or Fullscreen, match the in-game resolution to the
Windows desktop resolution. Unusual aspect ratios, custom display scaling, and
non-default HDR UI brightness have not been fully tested.

## Configuration

Keep the application in a writable folder. It creates and uses the following
runtime files beside the executable:

- `data/config.toml` — hotkeys, overlay settings, and optional behavior.
- `data/presets.json` — saved preset metadata.
- `data/local_templates/` — icon samples captured with each preset.
- `data/app.log` — diagnostic log from the latest launch.

The default keys can be changed in `data/config.toml` if they conflict with
another application or system shortcut. Presets are normally saved through the
in-game controls and do not need to be edited manually.

Hotkey keys support `f1`–`f12`, `0`–`9` (top row), and `numpad0`–`numpad9`.
For example, single-key numpad shortcuts can be configured as:

```toml
[hotkey]
modifiers = []
keys = ["numpad1", "numpad2", "numpad3", "numpad4", "numpad5", "numpad6"]
```

Keep Num Lock on and avoid Shift with numpad keys. Shortcuts are active only
while Helldivers 2 is in the foreground. Empty modifiers disable
hold-to-preview; action status still appears when the overlay is enabled.
Restart the application after changing hotkeys.

Selection order and automatic ready-up can also be toggled from the tray menu.

Use **Overlay monitor** in the tray menu to follow the game or choose a fixed display.

Optional overlay labels can be added in `data/config.toml`:

```toml
[presets.labels]
preset_1 = "Terminids"
preset_2 = "Automatons"
```

Empty labels are not shown. Labels beyond the current number of preset hotkeys are
kept for later.

## Troubleshooting

If a preset key does nothing, verify that the app is running in the tray, the
stable loadout home screen is open, all four Stratagem slots are filled or
empty, Helldivers 2 is in the foreground, and no other application uses the
same shortcut.

For bug reports, include `data/app.log`, the resolution and display mode,
Windows scaling and HDR status, and a screenshot of the affected screen.

## Building from source

Normal users should use the prebuilt ZIP from GitHub Releases. For development,
install a current stable Rust toolchain and run:

```powershell
cargo build --release --locked
```

The executable is written to `target\release\HD2PresetHelper.exe`.

## Legal

This is an unofficial third-party utility and is not affiliated with or
endorsed by Arrowhead Game Studios or Sony Interactive Entertainment.

The source code is licensed under the
[GNU General Public License version 3 or later](LICENSE).
