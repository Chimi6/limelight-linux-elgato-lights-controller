//! One background thread for every read / management call to the daemon.
//!
//! Jobs run strictly in order on a single thread, so there is never a pile
//! of concurrent fetches, and the UI thread never blocks on the network.
//! Results are handed back to the UI thread through `slint::invoke_from_event_loop`.
//!
//! The `apply` continuation stays on the UI thread (it may capture `Rc`s);
//! only the job and its `Send` result cross threads.

use crate::api::ApiClient;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

pub type Job = Box<dyn FnOnce(&ApiClient) + Send + 'static>;

type Apply = Box<dyn FnOnce(Box<dyn Any>) + 'static>;

thread_local! {
    /// Continuations waiting for their job's result, keyed by job id.
    /// Lives on the UI thread only.
    static PENDING: RefCell<HashMap<u64, Apply>> = RefCell::new(HashMap::new());
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Fetcher {
    tx: mpsc::Sender<Job>,
}

impl Fetcher {
    pub fn spawn(api: ApiClient) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("api-fetch".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&api);
                }
            })
            .expect("failed to spawn fetch thread");
        Self { tx }
    }

    /// Run `work` on the fetch thread, then `apply` with its result on the UI
    /// thread. Must be called from the UI thread.
    pub fn run<T, W, A>(&self, work: W, apply: A)
    where
        T: Send + 'static,
        W: FnOnce(&ApiClient) -> T + Send + 'static,
        A: FnOnce(T) + 'static,
    {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let apply: Apply = Box::new(move |any: Box<dyn Any>| {
            if let Ok(value) = any.downcast::<T>() {
                apply(*value);
            }
        });
        PENDING.with(|p| {
            p.borrow_mut().insert(id, apply);
        });

        let job: Job = Box::new(move |api| {
            let result: Box<dyn Any + Send> = Box::new(work(api));
            let _ = slint::invoke_from_event_loop(move || {
                let apply = PENDING.with(|p| p.borrow_mut().remove(&id));
                if let Some(apply) = apply {
                    apply(result);
                }
            });
        });
        let _ = self.tx.send(job);
    }

    /// Fire-and-forget: run `work` on the fetch thread, nothing comes back.
    pub fn fire<W>(&self, work: W)
    where
        W: FnOnce(&ApiClient) + Send + 'static,
    {
        let job: Job = Box::new(work);
        let _ = self.tx.send(job);
    }
}
