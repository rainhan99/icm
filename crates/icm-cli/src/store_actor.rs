//! Dedicated store-owning thread (F-003c).
//!
//! The blocking `postgres` client (icm-store) drives its connection with an
//! internal `Runtime::block_on`, which **panics** whenever it runs on a
//! thread that already has a tokio runtime context entered — and that
//! includes both async workers AND `tokio::task::spawn_blocking` pool threads
//! (`Handle::current()` succeeds on both). So the axum HTTP/web servers
//! cannot call the sync store directly from their handlers.
//!
//! This module runs the store on ONE dedicated `std::thread` that tokio does
//! not manage (no runtime context), so `block_on` works there. Handlers hold
//! a cloneable [`StoreHandle`], submit a job (a `FnOnce(&Store, Option<&dyn
//! Embedder>) -> T`) over a channel, and `.await` the result via a oneshot.
//! The single thread processes one job at a time — it is the serialization
//! point (replacing the old `Arc<Mutex<Store>>`) and preserves the F-003b
//! "set_tenant + query in one job" invariant.

use std::panic::AssertUnwindSafe;
use std::sync::mpsc;

use icm_core::Embedder;
use icm_store::Store;

/// A unit of work for the actor thread: runs against the owned store +
/// embedder. Type-erased so different result types share one channel.
type Job = Box<dyn FnOnce(&Store, Option<&dyn Embedder>) + Send + 'static>;

/// The store actor is unreachable: its thread is gone, or the submitted job
/// panicked (dropping the result channel).
#[derive(Debug)]
pub struct ActorError;

impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("store actor unavailable (thread gone or job panicked)")
    }
}

impl std::error::Error for ActorError {}

/// Cloneable handle to the dedicated store-actor thread.
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::Sender<Job>,
}

impl StoreHandle {
    /// Spawn the actor thread. It OWNS `store` and `embedder` on a plain
    /// `std::thread` (no tokio runtime context, so the blocking store client's
    /// `block_on` works) and serves jobs until every handle is dropped.
    pub fn spawn(
        store: Store,
        embedder: Option<Box<dyn Embedder + Send + Sync>>,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("icm-store-actor".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let emb = embedder.as_deref().map(|e| e as &dyn Embedder);
                    // Contain a job panic: one bad request must not kill the
                    // actor (which would hang every later request forever).
                    // The job's result sender drops on unwind → that caller
                    // gets an ActorError; the loop continues.
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(&store, emb)));
                }
            })?;
        Ok(StoreHandle { tx })
    }

    /// Run `f` on the actor thread and await its result. Returns [`ActorError`]
    /// if the actor is gone (channel closed) or the job panicked.
    pub async fn run<T, F>(&self, f: F) -> Result<T, ActorError>
    where
        F: FnOnce(&Store, Option<&dyn Embedder>) -> T + Send + 'static,
        T: Send + 'static,
    {
        let (otx, orx) = tokio::sync::oneshot::channel::<T>();
        let job: Job = Box::new(move |store, emb| {
            // If the receiver was dropped (caller went away), ignore.
            let _ = otx.send(f(store, emb));
        });
        self.tx.send(job).map_err(|_| ActorError)?;
        orx.await.map_err(|_| ActorError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use icm_core::MemoryStore;

    #[tokio::test(flavor = "multi_thread")]
    async fn actor_runs_survives_panic() {
        let store = Store::in_memory().unwrap();
        let h = StoreHandle::spawn(store, None).unwrap();
        // A normal job runs on the actor thread and returns its value.
        assert_eq!(h.run(|st, _| st.count().unwrap()).await.unwrap(), 0);
        // A panicking job returns an error and does NOT kill the actor.
        assert!(h.run(|_, _| panic!("boom")).await.is_err());
        // The actor is still alive and serves the next job.
        assert_eq!(h.run(|st, _| st.count().unwrap()).await.unwrap(), 0);
    }
}
