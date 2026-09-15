# Changelog

All notable changes to LimeLight. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

## [0.3.0] - 2026-09-15

### Added
- Presets: save a look from any light, group or All Lights card, apply it with one tap, and see which preset each light is on. Add, edit, rename, reorder and delete presets in Settings. Presets are stored by the daemon so the OpenDeck plugin can share them.
- OpenDeck plugin link in the README and app metadata.

### Fixed
- Dragging a brightness or temperature slider no longer scrolls the page.

## [0.2.0] - 2026-09-14

### Added
- Per-light settings: device name, power-on behaviour, default brightness and temperature, on/off fade durations, and an Identify button to find a light.
- Lights that go offline stay in the list, greyed out, and come back on their own.
- Light state refreshes automatically while the window is open, so changes made elsewhere show up.
- Setting to control whether newly found lights appear automatically.
- Light Strip and Key Light Mini are recognised by the daemon (colour and battery UI to follow).

### Changed
- Groups and "All Lights" switch every light at the same time instead of one after another.
- Lights keep their identity across renames and IP changes; existing setups migrate automatically.
- The window opens instantly and connects in the background.
- "All Lights" and group cards show the real average of their lights.
- Much smaller daemon and fewer dependencies.
- Flatpak is now published as a bundle on the Releases page; an AppImage is built alongside it.

### Removed
- The old tray app.

### Fixed
- Start on login did not work from the Flatpak.
- A light that was off when the app started never appeared until Manage Lights was opened.
- Deleting a light left it inside groups.

## [0.1.5] - 2026-02-22
- Slint GUI replaces the tray app; taskbar icon fix, manage lights/groups, settings, autostart.

## [0.1.4] - 2026-02-05
- Flathub metadata polish (screenshots, VCS URL).

[Unreleased]: https://github.com/Chimi6/limelight-linux-elgato-lights-controller/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Chimi6/limelight-linux-elgato-lights-controller/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Chimi6/limelight-linux-elgato-lights-controller/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/Chimi6/limelight-linux-elgato-lights-controller/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/Chimi6/limelight-linux-elgato-lights-controller/releases/tag/v0.1.4
