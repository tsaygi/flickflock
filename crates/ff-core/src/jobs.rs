//! A small worker pool driven by a *wanted list* instead of a FIFO queue.
//!
//! The owner repeatedly declares "these are the jobs I want, most important
//! first". Pending jobs that are no longer wanted simply disappear, running
//! jobs that are no longer wanted get their cancel flag set. This keeps the
//! workers busy only with what is on screen (or about to be) right now.

use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative cancellation flag handed to every job.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

struct State<K, J> {
    pending: VecDeque<(K, J)>,
    running: HashMap<K, CancelToken>,
    shutdown: bool,
}

struct Inner<K, J> {
    state: Mutex<State<K, J>>,
    wake: Condvar,
}

pub struct JobQueue<K, J> {
    inner: Arc<Inner<K, J>>,
}

impl<K, J> JobQueue<K, J>
where
    K: Clone + Eq + Hash + Send + 'static,
    J: Send + 'static,
{
    /// Start `threads` workers that run `run(key, job, cancel)` for each wanted job.
    pub fn new(
        name: &str,
        threads: usize,
        run: impl Fn(&K, J, &CancelToken) + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                pending: VecDeque::new(),
                running: HashMap::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        let run = Arc::new(run);
        for i in 0..threads.max(1) {
            let inner = inner.clone();
            let run = run.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("{name}-{i}"))
                .spawn(move || worker(&inner, &*run));
            if let Err(e) = spawned {
                tracing::error!("could not start worker thread: {e}");
            }
        }
        Self { inner }
    }

    /// Replace the wanted list (highest priority first). Jobs whose key is
    /// already running are not queued again; running jobs whose key is not in
    /// the list are cancelled.
    pub fn set_wanted(&self, jobs: Vec<(K, J)>) {
        let mut st = self.inner.state.lock();
        for (key, token) in &st.running {
            if !jobs.iter().any(|(k, _)| k == key) {
                token.cancel();
            }
        }
        let mut pending = VecDeque::with_capacity(jobs.len());
        for (k, j) in jobs {
            let running_live = st
                .running
                .get(&k)
                .map(|t| !t.is_cancelled())
                .unwrap_or(false);
            if !running_live && !pending.iter().any(|(pk, _)| *pk == k) {
                pending.push_back((k, j));
            }
        }
        st.pending = pending;
        drop(st);
        self.inner.wake.notify_all();
    }

    /// Is a job for `key` queued or (uncancelled) running?
    pub fn is_active(&self, key: &K) -> bool {
        let st = self.inner.state.lock();
        st.pending.iter().any(|(k, _)| k == key)
            || st
                .running
                .get(key)
                .map(|t| !t.is_cancelled())
                .unwrap_or(false)
    }

    /// Number of queued plus running jobs.
    pub fn load(&self) -> usize {
        let st = self.inner.state.lock();
        st.pending.len() + st.running.len()
    }
}

impl<K, J> Drop for JobQueue<K, J> {
    fn drop(&mut self) {
        let mut st = self.inner.state.lock();
        st.shutdown = true;
        st.pending.clear();
        for token in st.running.values() {
            token.cancel();
        }
        drop(st);
        self.inner.wake.notify_all();
    }
}

fn worker<K, J>(inner: &Inner<K, J>, run: &(dyn Fn(&K, J, &CancelToken) + Send + Sync))
where
    K: Clone + Eq + Hash,
{
    loop {
        let (key, job, token) = {
            let mut st = inner.state.lock();
            loop {
                if st.shutdown {
                    return;
                }
                // Take the first pending job whose key is not being worked on
                // (a cancelled run of the same key may still be winding down).
                let next = st
                    .pending
                    .iter()
                    .position(|(k, _)| !st.running.contains_key(k));
                if let Some((key, job)) = next.and_then(|i| st.pending.remove(i)) {
                    let token = CancelToken::default();
                    st.running.insert(key.clone(), token.clone());
                    break (key, job, token);
                }
                inner.wake.wait(&mut st);
            }
        };
        // A panicking job must not take the worker (or the running-set entry) with it.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&key, job, &token)));
        if result.is_err() {
            tracing::error!("a background job panicked");
        }
        inner.state.lock().running.remove(&key);
        // A pending job for the same key may have been waiting for this run to finish.
        inner.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn runs_wanted_jobs_in_priority_order() {
        let (tx, rx) = mpsc::channel();
        let q = JobQueue::new("t", 1, move |k: &u32, _: (), _: &CancelToken| {
            tx.send(*k).unwrap();
        });
        q.set_wanted(vec![(1, ()), (2, ()), (3, ())]);
        let got: Vec<u32> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn unwanted_pending_jobs_are_dropped_and_running_ones_cancelled() {
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let q = JobQueue::new("t", 1, move |k: &u32, _: (), cancel: &CancelToken| {
            started_tx.send(*k).unwrap();
            if *k == 1 {
                // Simulates a long decode that polls its token.
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            done_tx.send((*k, cancel.is_cancelled())).unwrap();
        });
        q.set_wanted(vec![(1, ()), (2, ()), (3, ())]);
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        assert!(q.is_active(&1) && q.is_active(&2));
        // The user flicked on: only 9 matters now.
        q.set_wanted(vec![(9, ())]);
        assert!(!q.is_active(&1) && !q.is_active(&2) && q.is_active(&9));
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (1, true)
        );
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (9, false)
        );
        assert!(
            started_rx.try_iter().all(|k| k == 9),
            "2 and 3 never started"
        );
    }

    #[test]
    fn survives_panicking_jobs() {
        let (tx, rx) = mpsc::channel();
        let q = JobQueue::new("t", 2, move |k: &u32, _: (), _: &CancelToken| {
            if *k == 1 {
                panic!("boom");
            }
            tx.send(*k).unwrap();
        });
        q.set_wanted(vec![(1, ()), (2, ())]);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        q.set_wanted(vec![(3, ())]);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
        // Wait for the pool to drain.
        for _ in 0..500 {
            if q.load() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("queue did not drain");
    }

    #[test]
    fn a_key_can_be_wanted_again_after_it_finished() {
        let (tx, rx) = mpsc::channel();
        let q = JobQueue::new("t", 2, move |k: &u32, _: (), _: &CancelToken| {
            tx.send(*k).unwrap();
        });
        q.set_wanted(vec![(1, ())]);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        q.set_wanted(vec![]);
        q.set_wanted(vec![(1, ())]);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    }
}
