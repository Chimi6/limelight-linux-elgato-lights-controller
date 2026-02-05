# LimeLight

Lightweight Elgato Key Light control for Linux.

I recently started dual booting Linux (Bazzite/KDE) and there’s no official Elgato Key Light “Control Center” equivalent here. So far the only thing missing from mimicing my windows set up.
Additonally… Control Center on Windows was a resource hog for me (and it loved to freeze), so I built my own.

LimeLight is split into two parts:

- **`keylightd` (daemon)**: discovers lights on your LAN, persists them, and talks to the Elgato local API.
- **`keylight-tray` (desktop UI)**: a small GUI that controls lights via the local daemon API.

## Features

- **mDNS discovery** of Key Lights (`_elg._tcp`)
- **Power / brightness / color temperature**
- **Groups** and **All Lights** control
- **Aliases** (friendly names) + persistence
- **Local HTTP API** so you can build third-party tools/plugins (Open Deck plugin coming)

## Quickstart (dev)

From the repo root:

```bash
cd helper
source "$HOME/.cargo/env"  # if cargo isn't on PATH
```

Run the daemon:

```bash
cargo run -p keylightd -- serve --port 9124
```

Run the UI (in a second terminal):

```bash
cargo run -p keylight-tray
```

## API

See `docs/API.md`.

## License

MPL-2.0 (see `LICENSE`).
