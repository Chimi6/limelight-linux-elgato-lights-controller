//! Batches slider / power changes and sends them to the daemon.
//!
//! Idle: blocked on `recv()`, zero CPU. On the first command it collects for
//! 50 ms, merging every field per target (so a power toggle followed by a
//! drag is *not* lost), then sends one request per target. The daemon fans
//! each request out to its lights in parallel, so "All" is a single call.
//! Every result is reported back so the UI can reconcile.

use crate::api::{ApiClient, ApiError};
use limelight_core::api::{UpdateRequest, UpdateResponse};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const BATCH_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpdateTarget {
    Light(String),
    Group(String),
    All,
}

pub struct UpdateCommand {
    pub target: UpdateTarget,
    pub update: UpdateRequest,
}

pub type ResultHandler =
    Arc<dyn Fn(UpdateTarget, Result<UpdateResponse, ApiError>) + Send + Sync + 'static>;

pub fn spawn(api: ApiClient, on_result: ResultHandler) -> mpsc::Sender<UpdateCommand> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("light-updater".into())
        .spawn(move || worker(rx, api, on_result))
        .expect("failed to spawn updater thread");
    tx
}

fn worker(rx: mpsc::Receiver<UpdateCommand>, api: ApiClient, on_result: ResultHandler) {
    while let Ok(first) = rx.recv() {
        let mut pending: HashMap<UpdateTarget, UpdateRequest> = HashMap::new();
        merge(&mut pending, first);

        let deadline = Instant::now() + BATCH_INTERVAL;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(cmd) => merge(&mut pending, cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        for (target, update) in pending.drain() {
            let result = match &target {
                UpdateTarget::Light(id) => api.update_light(id, &update),
                UpdateTarget::All => api.update_all(&update),
                UpdateTarget::Group(name) => api.update_group(name, &update),
            };
            on_result(target, result);
        }
    }
}

fn merge(map: &mut HashMap<UpdateTarget, UpdateRequest>, cmd: UpdateCommand) {
    let entry = map.entry(cmd.target).or_default();
    if cmd.update.on.is_some() {
        entry.on = cmd.update.on;
    }
    if cmd.update.brightness.is_some() {
        entry.brightness = cmd.update.brightness;
    }
    if cmd.update.kelvin.is_some() {
        entry.kelvin = cmd.update.kelvin;
        entry.mired = None;
    }
    if cmd.update.mired.is_some() {
        entry.mired = cmd.update.mired;
    }
    if cmd.update.hue.is_some() {
        entry.hue = cmd.update.hue;
    }
    if cmd.update.saturation.is_some() {
        entry.saturation = cmd.update.saturation;
    }
}
