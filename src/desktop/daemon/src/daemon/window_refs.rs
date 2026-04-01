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
const WINDOW_APP_PREFIX_MAX_LEN: usize = 32;

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
    let app_prefix = normalized_app_prefix(&window.app);
    let ref_id = loop {
        // Opaque short id for CLI ergonomics; retry if collision exists in live buffer.
        let suffix = Uuid::new_v4().simple().to_string()[..WINDOW_ID_LEN].to_string();
        let candidate = format!("{app_prefix}_{suffix}");
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

pub(crate) fn resolve_native_for_ref(reference: &str) -> Option<(i64, String)> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut store = lock_store();
    purge_expired(&mut store);
    let entry = store.by_ref.get_mut(trimmed)?;
    entry.touched_at = Instant::now();
    Some((entry.pid, entry.window_id.clone()))
}

fn normalized_app_prefix(app: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = false;

    for c in app.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if !last_was_sep && !out.is_empty() {
            out.push('_');
            last_was_sep = true;
        }
        if out.len() >= WINDOW_APP_PREFIX_MAX_LEN {
            break;
        }
    }

    while out.ends_with('_') {
        out.pop();
    }

    if out.is_empty() {
        "window".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::protocol::Bounds;

    fn sample_window(app: &str) -> WindowInfo {
        WindowInfo {
            id: "native-1".to_string(),
            window_ref: None,
            parent_id: None,
            pid: 123,
            index: 0,
            app: app.to_string(),
            title: "title".to_string(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            frontmost: true,
            visible: true,
            modal: None,
        }
    }

    #[test]
    fn normalizes_app_name_for_window_ref_prefix() {
        assert_eq!(normalized_app_prefix("System Settings"), "system_settings");
        assert_eq!(normalized_app_prefix(" Xcode 16.2 "), "xcode_16_2");
        assert_eq!(normalized_app_prefix("   "), "window");
    }

    #[test]
    fn issued_window_ref_uses_app_prefix_and_short_suffix() {
        let window = sample_window("System Settings");
        let issued = issue_for_window(&window);
        let parts: Vec<&str> = issued.split('_').collect();
        assert!(parts.len() >= 2);
        assert_eq!(parts.last().map(|s| s.len()), Some(WINDOW_ID_LEN));
        assert!(
            parts
                .last()
                .is_some_and(|s| s.chars().all(|c| c.is_ascii_hexdigit()))
        );
        assert!(issued.starts_with("system_settings_"));
    }

    #[test]
    fn resolves_native_window_from_ref() {
        let window = sample_window("System Settings");
        let issued = issue_for_window(&window);
        let resolved = resolve_native_for_ref(&issued);
        assert_eq!(resolved, Some((123, "native-1".to_string())));
    }
}
