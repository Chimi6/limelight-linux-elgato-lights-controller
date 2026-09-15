//! keylightd — LimeLight daemon and CLI.
//!
//! `keylightd serve` runs the localhost API used by the LimeLight GUI (and,
//! later, an Open Deck plugin). The other subcommands are thin CLI helpers
//! that operate on the same config file.

mod discovery;
mod server;
mod state;

use clap::{Parser, Subcommand};
use limelight_core::api::UpdateRequest;
use limelight_core::convert::mired_to_kelvin;
use state::{AppState, Target};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "keylightd",
    version,
    about = "LimeLight daemon for Elgato lights"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the localhost HTTP API (what the GUI talks to)
    Serve {
        /// Port to bind on 127.0.0.1 (env LIMELIGHT_PORT overrides the default)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Scan the LAN for Elgato lights and persist them
    Discover {
        /// Seconds to wait for responses
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    /// Show persisted lights
    List,
    /// Current state of a light
    Get {
        /// Light id, alias or name
        id: String,
    },
    /// Device info (accessory-info) of a light
    Info {
        /// Light id, alias or name
        id: String,
    },
    /// Make a light blink so you can find it
    Identify {
        /// Light id, alias or name
        id: String,
    },
    /// Update one light, a group, or all lights
    Set {
        /// Light id, alias or name
        #[arg(long)]
        id: Option<String>,
        /// Group name
        #[arg(long)]
        group: Option<String>,
        /// All enabled lights
        #[arg(long, default_value_t = false)]
        all: bool,
        /// 0 = off, 1 = on
        #[arg(long)]
        on: Option<u8>,
        /// Brightness percent (0-100)
        #[arg(long)]
        brightness: Option<u8>,
        /// Colour temperature in Kelvin (2900-7000)
        #[arg(long)]
        kelvin: Option<u16>,
        /// Colour temperature in mired (143-344)
        #[arg(long)]
        mired: Option<u16>,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let state = AppState::load().map_err(|e| e.to_string())?;

    match cli.command {
        Command::Serve { port } => {
            let port = port.unwrap_or_else(limelight_core::daemon_port);
            discovery::spawn_browser(state.clone());
            server::run(state, port)
        }
        Command::Discover { timeout } => {
            let timeout = timeout.clamp(1, 30);
            let found = discovery::scan_once(&state, Duration::from_secs(timeout))?;
            if found == 0 {
                println!("No Elgato lights found within {timeout}s.");
            } else {
                println!("Found {found} light(s).");
                print_list(&state);
            }
            Ok(())
        }
        Command::List => {
            print_list(&state);
            Ok(())
        }
        Command::Get { id } => {
            let record = state
                .read(|c| c.find_light(&id).cloned())
                .ok_or_else(|| format!("No light found with id '{id}'"))?;
            let ip = record
                .primary_address()
                .ok_or_else(|| "Light has no known address".to_string())?;
            let s = state.client.get_state(ip).map_err(|e| e.to_string())?;
            println!(
                "{}: on={} brightness={} kelvin={}{}",
                record.display_name(),
                s.on,
                s.brightness,
                s.temperature.map(mired_to_kelvin).unwrap_or(0),
                match (s.hue, s.saturation) {
                    (Some(h), Some(sat)) => format!(" hue={h} saturation={sat}"),
                    _ => String::new(),
                }
            );
            Ok(())
        }
        Command::Info { id } => {
            let record = state
                .read(|c| c.find_light(&id).cloned())
                .ok_or_else(|| format!("No light found with id '{id}'"))?;
            let ip = record
                .primary_address()
                .ok_or_else(|| "Light has no known address".to_string())?;
            let info = state.client.accessory_info(ip).map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&info).unwrap_or_default()
            );
            Ok(())
        }
        Command::Identify { id } => {
            state.identify(&id)?;
            println!("Identify sent.");
            Ok(())
        }
        Command::Set {
            id,
            group,
            all,
            on,
            brightness,
            kelvin,
            mired,
        } => {
            let update = UpdateRequest {
                on,
                brightness,
                kelvin,
                mired,
                hue: None,
                saturation: None,
            };
            if update.is_empty() {
                return Err(
                    "set needs at least one of --on, --brightness, --kelvin, --mired".into(),
                );
            }
            let target = match (id, group, all) {
                (Some(id), None, false) => Target::Light(id),
                (None, Some(g), false) => Target::Group(g),
                (None, None, true) => Target::All,
                _ => return Err("Provide exactly one of --id, --group, --all".into()),
            };
            let targets = state.resolve_targets(&target)?;
            let response = state.apply(&targets, &AppState::to_device_update(&update));
            for r in &response.results {
                match &r.state {
                    Some(s) if r.ok => println!(
                        "{}: on={} brightness={} kelvin={}",
                        s.display_name(),
                        s.on,
                        s.brightness,
                        s.kelvin
                    ),
                    _ => println!(
                        "{}: FAILED ({})",
                        r.id,
                        r.error.as_deref().unwrap_or("unknown error")
                    ),
                }
            }
            if response.ok {
                Ok(())
            } else {
                Err("No light accepted the update".into())
            }
        }
    }
}

fn print_list(state: &AppState) {
    state.read(|c| {
        if c.lights.is_empty() {
            println!("No lights known. Run `keylightd discover`.");
            return;
        }
        for l in &c.lights {
            println!(
                "{:<28} id={} enabled={} addr={} product={}",
                l.display_name(),
                l.id,
                l.enabled,
                l.primary_address().unwrap_or("-"),
                l.product.as_deref().unwrap_or("-")
            );
        }
        if !c.groups.is_empty() {
            println!("Groups:");
            for g in &c.groups {
                println!("  {} -> {}", g.name, g.members.join(", "));
            }
        }
    });
}
