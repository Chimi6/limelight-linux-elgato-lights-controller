# Development

## Layout

```
helper/                      Cargo workspace (version is set once in [workspace.package])
  crates/limelight-core      config model + atomic persistence + migrations,
                             Elgato device client (ureq, no TLS), daemon API types, conversions
  crates/keylightd           daemon + CLI
  crates/keylight-gui        Slint desktop window (binary installed as `limelight`)
flatpak/                     manifests, desktop file, metainfo, screenshots
public/                      icon sources
docs/                        this file + API.md
```

## Daemon design

- **State**: config is loaded once and kept in memory; every change is written atomically (temp file + rename). If another process edits the file (the CLI), the daemon notices via mtime and reloads.
- **Identity**: lights are keyed `serial:<serialNumber>`. The mDNS instance name is the device's display name and changes on rename, so it is only a secondary key. Older configs (mDNS-name ids) are migrated on first load, groups included.
- **Discovery**: a persistent mDNS browser thread keeps listening for `_elg._tcp`; resolutions update addresses and liveness, removals mark lights unreachable. `POST /v1/lights/refresh` runs an extra one-shot scan.
- **Concurrency**: `tiny_http` with a 4-thread worker pool; per-light I/O fans out with `std::thread::scope`, so a group of N lights is N simultaneous requests. No async runtime.
- **Timeouts**: connect 750 ms, total 2.5 s per light. A LAN device that has not answered by then is offline.
- **Cache**: last known state per light so offline lights still render with their previous values.

## GUI design

- The window opens immediately; daemon probing, settings and the first snapshot run on a single background "fetch" thread and post results back with `slint::invoke_from_event_loop`.
- Slider / power changes go through `update_queue`: 50 ms coalescing per target, fields merged, one request per target, results reported back so unreachable lights are greyed out.
- State is polled every 10 s while the window is focused and on focus-in. Polled values are ignored for 1.5 s after the user touched a control.

## Running locally

```bash
cd helper
cargo run -p keylight-gui              # spawns target/debug/keylightd if no daemon is up
# or separately:
cargo run -p keylightd -- serve
```

Use `LIMELIGHT_PORT=9199` (and optionally `XDG_CONFIG_HOME=/tmp/x`) to run a dev instance next to an installed one.

## Code quality

```bash
cd helper
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

There is no test suite by choice.

## Releasing

The release is automated by `.github/workflows/release.yml`, triggered by pushing a `vX.Y.Z` tag:

1. Set `version` in `helper/Cargo.toml` (`[workspace.package]`).
2. Add a `## [X.Y.Z] - date` section to `CHANGELOG.md` (it becomes the release notes verbatim) and a `<release>` entry in the metainfo.
3. Update `tag:` in `flatpak/…yml` (the manual release manifest; CI builds from the checkout instead).
4. Commit to `main`, then:

```bash
git tag vX.Y.Z
git push origin main vX.Y.Z
```

The workflow refuses to run if the tag and the Cargo version disagree or the changelog section is missing. It produces and attaches:

- `LimeLight-x86_64.AppImage` (built on Ubuntu 22.04 for glibc compatibility, via `packaging/appimage.sh`)
- `LimeLight.flatpak` (bundle built from the `.local.yml` manifest)
- `LimeLight-X.Y.Z-x86_64.tar.gz` (plain binaries + desktop file + icon)

### Building the artifacts locally

```bash
cd helper && cargo build --release -p keylightd -p keylight-gui && cd ..
packaging/appimage.sh                     # -> LimeLight-x86_64.AppImage
flatpak-builder --force-clean --user --install build-dir flatpak/io.github.chimi6.limelight-linux-elgato-lights-controller.local.yml
flatpak build-bundle ~/.local/share/flatpak/repo LimeLight.flatpak io.github.chimi6.limelight-linux-elgato-lights-controller
```

`LIMELIGHT_OPEN_SETTINGS=1` opens the first light's settings panel 2.5 s after launch (handy for screenshots).
