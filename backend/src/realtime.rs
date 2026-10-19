//! Real-time games on the Arena: a fixed-rate server tick per room, and the
//! budgets that keep one busy room from starving the rest. A port of
//! `backend/app/realtime.py`.
//!
//! Mini Golf is turn-based: the server only acts when a shot arrives. Kart
//! Racing moves continuously, so the server runs a loop per room at a fixed
//! rate: each tick it applies the game's rules to what the players sent since
//! the last one and sends everyone ONE batched snapshot.
//!
//! Budgets, because the Arena is one process with in-memory rooms:
//!   - `max` loops at once across the whole process (CPU), held by one
//!     [`Registry`] that the app shares (the Python keeps a module global; a
//!     value here lets tests run their own small registry in parallel). A game
//!     that can't get a slot says so to the host instead of starting.
//!   - [`ROOM_BYTES_PER_SEC`] of outgoing snapshot traffic per room. Over it,
//!     the loop thins snapshots to every 2nd, then 3rd tick until the room is
//!     back under; the rules still run every tick.
//!   - [`Bucket`]: a token bucket for inbound frames.
//!
//! Nothing here knows about any one game; `kart.rs` is the first user.

// Bucket, get/stats and the Ticker readers are this module's API (as in the
// Python, where kart.py keeps its own inline bucket); only tests read some today.
#![cfg_attr(not(test), allow(dead_code))]

use futures::FutureExt;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::AbortHandle;

/// Real-time game loops running at once, process-wide.
pub const MAX_TICKERS: usize = 48;
/// Snapshot bytes a room may send per second (all sockets).
pub const ROOM_BYTES_PER_SEC: usize = 160 * 1024;
/// Thinnest snapshot rate under pressure: every 3rd tick.
pub const MAX_STRIDE: u64 = 3;
/// A tick this late (seconds) is counted as an overrun.
pub const SLOW_TICK: f64 = 0.25;

/// The game clock a loop hands to its step (seconds; patched in tests).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Token bucket: `burst` deep, refilled `rate` per second.
#[derive(Clone, Debug)]
pub struct Bucket {
    pub rate: f64,
    pub burst: f64,
    pub tokens: f64,
    pub at: f64,
}

impl Bucket {
    pub fn new(rate: f64, burst: f64, t: f64) -> Self {
        Self { rate, burst, tokens: burst, at: t }
    }

    pub fn take(&mut self, t: f64) -> bool {
        self.tokens = self.burst.min(self.tokens + (t - self.at).max(0.0) * self.rate);
        self.at = t;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[derive(Debug, Default)]
struct TickState {
    ticks: u64,
    overruns: u64,
    stride: u64,
    sent: VecDeque<(Instant, usize)>, // (when, bytes) over the last second
    bytes_1s: usize,
}

/// One room's loop. Shared between the registry, the loop task and the step.
pub struct Ticker {
    pub key: String,
    pub hz: f64,
    st: Mutex<TickState>,
    task: Mutex<Option<AbortHandle>>,
    done: AtomicBool,
    done_tx: Notify,
}

impl Ticker {
    fn new(key: &str, hz: f64) -> Self {
        Self {
            key: key.to_string(),
            hz,
            st: Mutex::new(TickState { stride: 1, ..Default::default() }),
            task: Mutex::new(None),
            done: AtomicBool::new(false),
            done_tx: Notify::new(),
        }
    }

    /// The step reports what it just sent; this feeds the bandwidth budget.
    pub fn count(&self, nbytes: usize) {
        let t = Instant::now();
        let mut s = self.st.lock().unwrap();
        s.sent.push_back((t, nbytes));
        s.bytes_1s += nbytes;
        while let Some(&(at, n)) = s.sent.front() {
            if t.duration_since(at).as_secs_f64() > 1.0 {
                s.bytes_1s -= n;
                s.sent.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn ticks(&self) -> u64 {
        self.st.lock().unwrap().ticks
    }
    pub fn overruns(&self) -> u64 {
        self.st.lock().unwrap().overruns
    }
    pub fn stride(&self) -> u64 {
        self.st.lock().unwrap().stride
    }
    pub fn bytes_1s(&self) -> usize {
        self.st.lock().unwrap().bytes_1s
    }

    pub fn is_running(&self) -> bool {
        !self.done.load(Ordering::SeqCst)
            && self.task.lock().unwrap().as_ref().map(|h| !h.is_finished()).unwrap_or(true)
    }

    /// Wait for the loop to end (its step said stop, it was stopped, or it panicked).
    pub async fn finished(&self) {
        loop {
            let n = self.done_tx.notified();
            if self.done.load(Ordering::SeqCst) {
                return;
            }
            n.await;
        }
    }

    /// Adjust the snapshot stride, then say whether this tick sends a snapshot.
    fn next_tick(&self) -> bool {
        let mut s = self.st.lock().unwrap();
        if s.bytes_1s > ROOM_BYTES_PER_SEC && s.stride < MAX_STRIDE {
            s.stride += 1;
        } else if s.bytes_1s < ROOM_BYTES_PER_SEC / 2 && s.stride > 1 {
            s.stride -= 1;
        }
        let send = s.ticks.is_multiple_of(s.stride);
        s.ticks += 1;
        send
    }
}

/// Runs when the loop's future ends or is dropped (aborted): the Python's `finally`.
struct Exit {
    reg: Arc<Registry>,
    tk: Arc<Ticker>,
}

impl Drop for Exit {
    fn drop(&mut self) {
        if let Ok(mut map) = self.reg.tickers.lock() {
            if map.get(&self.tk.key).map(|t| Arc::ptr_eq(t, &self.tk)).unwrap_or(false) {
                map.remove(&self.tk.key);
            }
        }
        self.tk.done.store(true, Ordering::SeqCst);
        self.tk.done_tx.notify_waiters();
    }
}

/// The process-wide set of running loops.
pub struct Registry {
    max: usize,
    tickers: Mutex<HashMap<String, Arc<Ticker>>>,
}

impl Registry {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self { max, tickers: Mutex::new(HashMap::new()) })
    }

    /// Start (or keep) the loop for `key` (e.g. "kart:<room>"). `step(t, send, ticker)`
    /// returns whether to keep running; `send` is false on a tick whose snapshot is
    /// thinned out by the bandwidth budget (the step still applies its rules and sends
    /// events). None when the process is at its budget or there is no tokio runtime.
    pub fn start<F, Fut>(self: &Arc<Self>, key: &str, hz: f64, mut step: F, clock: Clock)
        -> Option<Arc<Ticker>>
    where
        F: FnMut(f64, bool, Arc<Ticker>) -> Fut + Send + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        let rt = tokio::runtime::Handle::try_current().ok()?;
        let mut map = self.tickers.lock().unwrap();
        if let Some(tk) = map.get(key) {
            if tk.is_running() {
                return Some(tk.clone());
            }
        }
        if map.len() >= self.max && !map.contains_key(key) {
            return None;
        }
        let tk = Arc::new(Ticker::new(key, hz));
        map.insert(key.to_string(), tk.clone());
        let exit = Exit { reg: self.clone(), tk: tk.clone() };
        let me = tk.clone();
        let handle = rt.spawn(async move {
            let _exit = exit;
            let period = Duration::from_secs_f64(1.0 / hz);
            let mut due = tokio::time::Instant::now();
            loop {
                due += period;
                let send = me.next_tick();
                // A bug in one game's tick must not take the loop down silently.
                let made = std::panic::catch_unwind(AssertUnwindSafe(|| step(clock(), send, me.clone())));
                let keep = match made {
                    Ok(fut) => AssertUnwindSafe(fut).catch_unwind().await.unwrap_or(false),
                    Err(_) => false,
                };
                if !keep {
                    return;
                }
                let now = tokio::time::Instant::now();
                if now > due && (now - due).as_secs_f64() > SLOW_TICK {
                    me.st.lock().unwrap().overruns += 1;
                    due = now; // don't burst to catch up: skip the missed ticks
                }
                tokio::time::sleep_until(due).await;
            }
        });
        *tk.task.lock().unwrap() = Some(handle.abort_handle());
        drop(map);
        Some(tk)
    }

    pub fn get(&self, key: &str) -> Option<Arc<Ticker>> {
        self.tickers.lock().unwrap().get(key).cloned()
    }

    /// Stop the loop for `key`. From inside its own step this only unregisters it
    /// (the step then returns false), as in the Python.
    pub fn stop(&self, key: &str) {
        let tk = self.tickers.lock().unwrap().remove(key);
        if let Some(tk) = tk {
            if let Some(h) = tk.task.lock().unwrap().as_ref() {
                if tokio::task::try_id() != Some(h.id()) {
                    h.abort();
                }
            }
        }
    }

    pub fn running(&self) -> usize {
        self.tickers.lock().unwrap().len()
    }

    pub fn stats(&self) -> Value {
        let map = self.tickers.lock().unwrap();
        let rooms: serde_json::Map<String, Value> = map
            .iter()
            .map(|(k, t)| {
                (k.clone(), json!({"hz": t.hz, "ticks": t.ticks(), "overruns": t.overruns(),
                                   "stride": t.stride(), "bytesPerSec": t.bytes_1s()}))
            })
            .collect();
        json!({"running": map.len(), "max": self.max, "rooms": rooms})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn mono() -> Clock {
        let t0 = Instant::now();
        Arc::new(move || t0.elapsed().as_secs_f64())
    }

    #[test]
    fn bucket() {
        let mut b = Bucket::new(10.0, 3.0, 0.0);
        let took: Vec<bool> = (0..4).map(|_| b.take(0.0)).collect();
        assert_eq!(took, [true, true, true, false]);
        assert!(b.take(0.1) && !b.take(0.1));
    }

    #[tokio::test]
    async fn ticker_runs_thins_and_stops() {
        let reg = Registry::new(MAX_TICKERS);
        let seen = Arc::new(Mutex::new(Vec::<bool>::new()));
        let s2 = seen.clone();
        let step = move |_t: f64, send: bool, tk: Arc<Ticker>| {
            let seen = s2.clone();
            async move {
                let mut v = seen.lock().unwrap();
                v.push(send);
                tk.count(ROOM_BYTES_PER_SEC); // always over budget
                v.len() < 12
            }
        };
        let tk = reg.start("t:1", 200.0, step.clone(), mono()).expect("a slot");
        let again = reg.start("t:1", 200.0, step, mono()).unwrap();
        assert!(Arc::ptr_eq(&tk, &again));
        tokio::time::timeout(Duration::from_secs(2), tk.finished()).await.expect("loop ended");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 12);
        assert!(reg.get("t:1").is_none());
        assert_eq!(tk.stride(), MAX_STRIDE);
        assert!(seen.iter().filter(|s| !**s).count() >= 4);
    }

    #[tokio::test]
    async fn ticker_budget_is_process_wide() {
        let reg = Registry::new(2);
        let step = |_t: f64, _s: bool, _tk: Arc<Ticker>| async {
            tokio::task::yield_now().await;
            true
        };
        let a = reg.start("t:a", 50.0, step, mono());
        let b = reg.start("t:b", 50.0, step, mono());
        assert!(a.is_some() && b.is_some());
        assert!(reg.start("t:c", 50.0, step, mono()).is_none());
        reg.stop("t:a");
        reg.stop("t:b");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(reg.stats()["running"], 0);
        assert!(a.unwrap().finished().now_or_never().is_some());
        // The freed slots can be taken again.
        assert!(reg.start("t:c", 50.0, step, mono()).is_some());
        reg.stop("t:c");
    }

    #[tokio::test]
    async fn a_panicking_step_ends_its_loop_and_frees_the_slot() {
        let reg = Registry::new(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let tk = reg
            .start("t:p", 100.0, move |_t, _s, _tk| {
                let c = c2.clone();
                async move {
                    if c.fetch_add(1, Ordering::SeqCst) == 2 {
                        panic!("bug in a game");
                    }
                    true
                }
            }, mono())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), tk.finished()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(reg.running(), 0);
    }

    #[tokio::test]
    async fn the_step_sees_the_given_clock() {
        let reg = Registry::new(4);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let tk = reg
            .start("t:clk", 100.0, move |t, _s, _tk| {
                let g = g2.clone();
                async move {
                    g.lock().unwrap().push(t);
                    false
                }
            }, Arc::new(|| 1234.5))
            .unwrap();
        tk.finished().await;
        assert_eq!(*got.lock().unwrap(), vec![1234.5]);
    }
}
