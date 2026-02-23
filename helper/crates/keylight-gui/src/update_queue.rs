use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::api::{ApiClient, UpdatePayload};

const BATCH_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpdateTarget {
    Light(String),
    All,
    Group(String),
}

pub enum UpdateCommand {
    SliderDrag {
        target: UpdateTarget,
        payload: UpdatePayload,
    },
    SliderRelease {
        target: UpdateTarget,
        payload: UpdatePayload,
    },
    PowerToggle {
        target: UpdateTarget,
        on: bool,
    },
}

/// Spawns a background thread that batches update commands.
///
/// When idle, the thread blocks on recv() (no busy-loop).
/// Once the first event arrives, it collects for 50ms — coalescing
/// by target so only the latest value per light/group survives —
/// then sends one blocking HTTP request per target.  This matches
/// the proven keylight-tray pattern: consistent ~20fps update cadence,
/// no flooding, and the light tracks the slider smoothly.
pub fn spawn(api: ApiClient) -> mpsc::Sender<UpdateCommand> {
    let (tx, rx) = mpsc::channel();

    thread::Builder::new()
        .name("light-updater".into())
        .spawn(move || worker(rx, api))
        .expect("failed to spawn updater thread");

    tx
}

fn worker(rx: mpsc::Receiver<UpdateCommand>, api: ApiClient) {
    loop {
        // Block until the first command arrives (idle = no CPU)
        let first = match rx.recv() {
            Ok(cmd) => cmd,
            Err(_) => break,
        };

        let mut pending: HashMap<UpdateTarget, UpdateCommand> = HashMap::new();
        coalesce(&mut pending, first);

        // Collect for 50ms, keeping only the latest value per target
        let deadline = Instant::now() + BATCH_INTERVAL;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(cmd) => coalesce(&mut pending, cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Send the latest value for each target
        for (_, cmd) in pending.drain() {
            execute(&api, cmd);
        }
    }
}

fn coalesce(map: &mut HashMap<UpdateTarget, UpdateCommand>, cmd: UpdateCommand) {
    let target = match &cmd {
        UpdateCommand::SliderDrag { target, .. }
        | UpdateCommand::SliderRelease { target, .. }
        | UpdateCommand::PowerToggle { target, .. } => target.clone(),
    };
    map.insert(target, cmd);
}

fn execute(api: &ApiClient, cmd: UpdateCommand) {
    match cmd {
        UpdateCommand::SliderDrag { target, payload }
        | UpdateCommand::SliderRelease { target, payload } => {
            let _ = fire(api, &target, &payload);
        }
        UpdateCommand::PowerToggle { target, on } => {
            let payload = UpdatePayload {
                on: Some(if on { 1 } else { 0 }),
                brightness: None,
                kelvin: None,
            };
            let _ = fire(api, &target, &payload);
        }
    }
}

fn fire(
    api: &ApiClient,
    target: &UpdateTarget,
    payload: &UpdatePayload,
) -> Result<(), reqwest::Error> {
    match target {
        UpdateTarget::Light(id) => api.update_light(id, payload),
        UpdateTarget::All => api.update_all(payload),
        UpdateTarget::Group(name) => api.update_group(name, payload),
    }
}
