use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use uuid::Uuid;

use crate::platform::windowing::WindowInfo;

const WINDOW_REF_TTL: Duration = Duration::from_secs(10 * 60);
const WINDOW_REF_MAX: usize = 1024;
const WINDOW_ID_LEN: usize = 6;

#[derive(Clone)]
struct Entry {
    pid: i64,
    window_id: String,
    touched_at: Instant,
}

#[derive(Default)]
struct Store {
    by_ref: HashMap<String, Entry>,
    by_key: HashMap<String, String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn lock_store() -> std::sync::MutexGuard<'static, Store> {
    let lock = STORE.get_or_init(|| Mutex::new(Store::default()));
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn key(pid: i64, window_id: &str) -> String {
    format!("{pid}:{window_id}")
}

fn purge_expired(store: &mut Store) {
    let now = Instant::now();
    let stale: Vec<String> = store
        .by_ref
        .iter()
        .filter(|(_, entry)| now.duration_since(entry.touched_at) > WINDOW_REF_TTL)
        .map(|(id, _)| id.clone())
        .collect();
    for ref_id in stale {
        if let Some(entry) = store.by_ref.remove(&ref_id) {
            store.by_key.remove(&key(entry.pid, &entry.window_id));
        }
    }
    if store.by_ref.len() <= WINDOW_REF_MAX {
        return;
    }
    let mut refs_by_age: Vec<(String, Instant)> = store
        .by_ref
        .iter()
        .map(|(id, entry)| (id.clone(), entry.touched_at))
        .collect();
    refs_by_age.sort_by_key(|(_, touched)| *touched);
    let overflow = refs_by_age.len().saturating_sub(WINDOW_REF_MAX);
    for (ref_id, _) in refs_by_age.into_iter().take(overflow) {
        if let Some(entry) = store.by_ref.remove(&ref_id) {
            store.by_key.remove(&key(entry.pid, &entry.window_id));
        }
    }
}

pub(crate) fn issue_for_window(window: &WindowInfo) -> String {
    let mut store = lock_store();
    purge_expired(&mut store);
    let native_key = key(window.pid, &window.id);
    if let Some(existing) = store.by_key.get(&native_key).cloned() {
        if let Some(entry) = store.by_ref.get_mut(&existing) {
            entry.touched_at = Instant::now();
        }
        return existing;
    }
    let ref_id = loop {
        // Opaque short id for CLI ergonomics; retry if collision exists in live buffer.
        let candidate = Uuid::new_v4().simple().to_string()[..WINDOW_ID_LEN].to_string();
        if !store.by_ref.contains_key(&candidate) {
            break candidate;
        }
    };
    store.by_key.insert(native_key, ref_id.clone());
    store.by_ref.insert(
        ref_id.clone(),
        Entry {
            pid: window.pid,
            window_id: window.id.clone(),
            touched_at: Instant::now(),
        },
    );
    ref_id
}
