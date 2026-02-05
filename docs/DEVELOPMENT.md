# Development

## Architecture

LimeLight is intentionally split:

- **`keylightd`**: a small Rust daemon that discovers and controls Elgato Key Lights and exposes a localhost HTTP API.
- **`keylight-tray`**: an `eframe`/`egui` desktop UI that calls the daemon API.

This keeps UI concerns separate from networking/discovery and makes it easy to build integrations (Open Deck plugin, scripts, etc).

## Where config is stored

`keylightd` persists discovered lights and groups in:

- `~/.config/limelight-keylight/config.json`

If an old config exists at `~/.config/limekit-keylight/config.json`, the daemon will migrate it automatically.

## Running locally

From the repo root:

```bash
cd helper
```

Run the daemon:

```bash
cargo run -p keylightd -- serve --port 9124
```

Run the UI:

```bash
cargo run -p keylight-tray
```

## Code quality

```bash
cd helper
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

