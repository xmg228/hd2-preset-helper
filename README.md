# HD2 Preset Helper

**English** | [简体中文](README.zh-CN.md)

HD2 Preset Helper is a Windows utility for saving and applying loadout presets in Helldivers 2. Presets can include Stratagems, a Booster, and equipment, with a preset panel and configurable hotkeys.

The program recognizes the game interface through screen capture and completes selections using standard mouse and keyboard input. It does not modify game files, inject code into the game process, read game memory, or alter network traffic.

## Download and install

1. Download the latest ZIP from [GitHub Releases](../../releases/latest).
2. Extract it to a separate writable folder.
3. Run `HD2PresetHelper.exe`.

To update, extract the new `HD2PresetHelper` folder over the existing folder.
Settings are preserved. If a release changes the preset format, the app backs
up the old preset file and asks you to recreate the presets.

The preset panel opens on startup. Hiding it leaves the application running
in the system tray. Left-click its tray icon to reopen the panel,
or right-click and select **Exit** to close the application.

To uninstall, exit the application and delete the extracted
`HD2PresetHelper` folder. This also removes its configuration, presets, and
logs.

## Quick start

1. Open the loadout home screen and select all four Stratagems and, optionally,
   a Booster. Equip the helmet, armor, cape and weapons you want to save.
2. Press `Ctrl+Shift+Space` to open the panel, select a preset, check both sections and click **Save preset**
   (or press `Ctrl+S`). Release all keys and mouse buttons to start.
3. To apply it later, open the panel from either loadout home and choose
   **Apply all**, or use `Ctrl+Shift+F7` through `Ctrl+Shift+F12` for presets 1–6.

Preset shortcuts **only apply**, never save. Existing bindings are kept on upgrade,
but the old context-sensitive save/apply behavior is replaced.

### Preset panel

`Ctrl+Shift+Space` toggles the panel from the game or desktop. Esc or switching to
another window hides it. Saving or applying brings the running game to the foreground;
release all keys and mouse buttons to let the action start.

Hover to preview; click a row or use Up/Down or W/S to select. Enter or double-click applies
the selected preset. Use **+ New preset** to add presets and the pencil or trash icon
beside the title to rename or delete them.

Check **Stratagems & Booster**, **Equipment**, or both to choose what to save and
apply. This choice is shared across presets, remembered across restarts, and also
used by preset shortcuts. **Saving overwrites the checked sections without confirmation**;
unchecked sections are preserved. Applying skips sections that have not been saved.

### Usage notes

- Start from either loadout home screen. Page switching uses the game's default `R` key.
- Saving Stratagems requires all four slots filled; saving only equipment does not.
  Applying keeps matching Stratagems in place, replaces the others, and fills empty
  slots. Saved-order selection only applies when all four slots start empty.
- Equipment includes helmet, armor, cape, primary, secondary and throwable, but not
  weapon attachments or separate appearances of items with the same name. Templates
  and the learned order cache work across resolutions; save equipment again after
  changing the game language.
- The first search in an equipment category scans it completely and may take longer.
  Later applications reuse the learned order.

## Compatibility

Windows 10 version 1903 or later and Windows 11 are supported. The app has been
tested from 720p to 2160p in Windowed, Borderless Window, and Fullscreen modes,
including several non-16:9 resolutions and Windows HDR.

For Borderless Window or Fullscreen, match the in-game resolution to the
Windows desktop resolution. Unusual aspect ratios, custom display scaling, and
non-default HDR UI brightness have not been fully tested.

Exclusive fullscreen may minimize when the panel takes focus. Use Borderless Window
if this occurs.

## Configuration

Open the panel's **Settings** to configure:

- **Shortcuts** — the panel shortcut and optional per-preset shortcuts, including
  single keys. The panel shortcut works globally; preset shortcuts work while the
  game or panel is in the foreground. Panel navigation keys take priority over
  custom bindings. For numpad digits, keep Num Lock on and avoid Shift combinations.
- **General** — saved selection order, automatic ready-up, fallback Booster saving,
  and the display used for the panel and status window.

Changes take effect when saved. To edit the file directly, see the comments in
`data/config.toml` and restart afterward.

All user data is kept in `data/` beside the executable. Back up this folder to keep
your settings and presets. To relearn the equipment order, exit the app and delete
only `data/equipment-cache.json`.

## Troubleshooting

If a preset key does nothing, verify that the app is running in the tray, the
stable loadout home screen is open, the game or preset panel is in the foreground,
you are not renaming or recording a shortcut, and no other application uses the same shortcut.

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
