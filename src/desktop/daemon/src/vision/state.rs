use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use desktop_core::{
    error::AppError,
    protocol::{Bounds, SnapshotDisplay, SnapshotPayload, TokenEntry, TokenizePayload},
};

use super::{diff::GrayThumbnail, types::CapturedFrame};

const MAX_EVENTS: usize = 512;
const MAX_FRAMES: usize = 64;
const TOKENIZE_CACHE_CAPACITY: usize = 4;

#[derive(Debug, Clone)]
pub struct VisionEvent {
    pub event_id: u64,
    pub snapshot_id: u64,
}

#[derive(Debug, Clone)]
pub struct CaptureUpdate {
    pub snapshot: SnapshotPayload,
    pub event_id: u64,
}

#[derive(Debug)]
pub struct VisionState {
    next_snapshot_id: u64,
    next_event_id: u64,
    latest_snapshot: Option<SnapshotPayload>,
    latest_frame_path: Option<PathBuf>,
    latest_frame_png: Option<Arc<[u8]>>,
    latest_thumbnail: Option<GrayThumbnail>,
    tokenize_cache: VecDeque<(String, u64, Arc<TokenizePayload>)>,
    events: VecDeque<VisionEvent>,
    frames: VecDeque<PathBuf>,
    token_map: HashMap<u32, TokenEntry>,
}

impl VisionState {
    pub fn new() -> Self {
        Self {
            next_snapshot_id: 1,
            next_event_id: 1,
            latest_snapshot: None,
            latest_frame_path: None,
            latest_frame_png: None,
            latest_thumbnail: None,
            tokenize_cache: VecDeque::new(),
            events: VecDeque::new(),
            frames: VecDeque::new(),
            token_map: HashMap::new(),
        }
    }

    pub fn latest_snapshot(&self) -> Option<SnapshotPayload> {
        self.latest_snapshot.clone()
    }

    pub fn latest_thumbnail(&self) -> Option<&GrayThumbnail> {
        self.latest_thumbnail.as_ref()
    }

    pub fn latest_frame_path(&self) -> Option<PathBuf> {
        self.latest_frame_path.clone()
    }

    pub fn latest_frame_png(&self) -> Option<Arc<[u8]>> {
        self.latest_frame_png.as_ref().map(Arc::clone)
    }

    pub fn cached_tokenize_payload_if_fingerprint(
        &self,
        cache_key: &str,
        fingerprint: u64,
    ) -> Option<Arc<TokenizePayload>> {
        self.tokenize_cache
            .iter()
            .find(|(key, cached_fingerprint, _)| {
                key == cache_key && *cached_fingerprint == fingerprint
            })
            .map(|(_, _, payload)| Arc::clone(payload))
    }

    pub fn update_tokenize_cache(
        &mut self,
        cache_key: String,
        fingerprint: u64,
        payload: Arc<TokenizePayload>,
    ) {
        if let Some((_, cached_fingerprint, cached_payload)) = self
            .tokenize_cache
            .iter_mut()
            .find(|(key, _, _)| *key == cache_key)
        {
            *cached_fingerprint = fingerprint;
            *cached_payload = payload;
            return;
        }
        self.tokenize_cache
            .push_back((cache_key, fingerprint, payload));
        while self.tokenize_cache.len() > TOKENIZE_CACHE_CAPACITY {
            self.tokenize_cache.pop_front();
        }
    }

    pub fn token_map(&self) -> &HashMap<u32, TokenEntry> {
        &self.token_map
    }

    pub fn replace_token_map(&mut self, tokens: Vec<TokenEntry>) {
        self.token_map.clear();
        for token in tokens {
            self.token_map.insert(token.n, token);
        }
    }

    pub fn record_capture(
        &mut self,
        capture: CapturedFrame,
        frame_png: Option<Arc<[u8]>>,
        thumbnail: GrayThumbnail,
        focused_app: Option<String>,
        texts: Vec<desktop_core::protocol::SnapshotText>,
        _roi: Option<Bounds>,
    ) -> CaptureUpdate {
        self.record_capture_internal(capture, frame_png, Some(thumbnail), focused_app, texts)
    }

    pub fn record_capture_without_diff_baseline(
        &mut self,
        capture: CapturedFrame,
        frame_png: Option<Arc<[u8]>>,
        focused_app: Option<String>,
        texts: Vec<desktop_core::protocol::SnapshotText>,
    ) -> CaptureUpdate {
        self.record_capture_internal(capture, frame_png, None, focused_app, texts)
    }

    fn record_capture_internal(
        &mut self,
        capture: CapturedFrame,
        frame_png: Option<Arc<[u8]>>,
        thumbnail: Option<GrayThumbnail>,
        focused_app: Option<String>,
        texts: Vec<desktop_core::protocol::SnapshotText>,
    ) -> CaptureUpdate {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id += 1;
        let event_id = self.next_event_id;
        self.next_event_id += 1;

        let snapshot = SnapshotPayload {
            snapshot_id,
            timestamp: capture.timestamp.clone(),
            display: SnapshotDisplay {
                id: capture.display_id,
                width: capture.width,
                height: capture.height,
                scale: capture.scale,
            },
            focused_app,
            texts,
        };

        self.latest_snapshot = Some(snapshot.clone());
        self.latest_frame_path = capture.image_path.clone();
        self.latest_frame_png = frame_png;
        if let Some(thumbnail) = thumbnail {
            self.latest_thumbnail = Some(thumbnail);
        }
        if let Some(path) = capture.image_path {
            self.frames.push_back(path);
        }
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
        }

        self.events.push_back(VisionEvent {
            event_id,
            snapshot_id,
        });
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }

        CaptureUpdate { snapshot, event_id }
    }

    pub fn event_ids(&self, snapshot_id: u64) -> Vec<u64> {
        self.events
            .iter()
            .filter(|evt| evt.snapshot_id == snapshot_id)
            .map(|evt| evt.event_id)
            .collect()
    }
}

static VISION_STATE: OnceLock<Mutex<VisionState>> = OnceLock::new();

pub fn with_state<T>(f: impl FnOnce(&mut VisionState) -> T) -> Result<T, AppError> {
    let state = VISION_STATE.get_or_init(|| Mutex::new(VisionState::new()));
    let mut guard = state
        .lock()
        .map_err(|_| AppError::internal("vision state lock poisoned"))?;
    Ok(f(&mut guard))
}

#[cfg(test)]
mod tests {
    use super::VisionState;
    use crate::vision::{diff::GrayThumbnail, types::CapturedFrame};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_tokenize_payload(snapshot_id: u64) -> Arc<desktop_core::protocol::TokenizePayload> {
        Arc::new(desktop_core::protocol::TokenizePayload {
            snapshot_id,
            timestamp: format!("ts-{snapshot_id}"),
            image: None,
            windows: Vec::new(),
        })
    }

    #[test]
    fn snapshot_ids_are_monotonic() {
        let mut state = VisionState::new();
        let thumb = GrayThumbnail {
            width: 2,
            height: 2,
            pixels: vec![0; 4],
        };
        let capture1 = CapturedFrame {
            snapshot_id: 1,
            timestamp: "t1".to_string(),
            display_id: 1,
            width: 100,
            height: 100,
            scale: 2.0,
            image_path: Some(PathBuf::from("/tmp/a.png")),
        };
        let capture2 = CapturedFrame {
            snapshot_id: 2,
            timestamp: "t2".to_string(),
            display_id: 1,
            width: 100,
            height: 100,
            scale: 2.0,
            image_path: Some(PathBuf::from("/tmp/b.png")),
        };
        let first = state.record_capture(capture1, None, thumb.clone(), None, Vec::new(), None);
        let second = state.record_capture(capture2, None, thumb, None, Vec::new(), None);
        assert!(first.snapshot.snapshot_id < second.snapshot.snapshot_id);
    }

    #[test]
    fn event_ids_are_monotonic() {
        let mut state = VisionState::new();
        let thumb = GrayThumbnail {
            width: 2,
            height: 2,
            pixels: vec![0; 4],
        };
        let base_capture = CapturedFrame {
            snapshot_id: 1,
            timestamp: "t1".to_string(),
            display_id: 1,
            width: 100,
            height: 100,
            scale: 2.0,
            image_path: Some(PathBuf::from("/tmp/a.png")),
        };
        let first = state.record_capture(
            base_capture.clone(),
            None,
            thumb.clone(),
            None,
            Vec::new(),
            None,
        );
        let second = state.record_capture(base_capture, None, thumb, None, Vec::new(), None);
        assert!(first.event_id < second.event_id);
    }

    #[test]
    fn scoped_capture_does_not_replace_diff_thumbnail() {
        let mut state = VisionState::new();
        let display_thumb = GrayThumbnail {
            width: 2,
            height: 2,
            pixels: vec![1; 4],
        };
        let display_capture = CapturedFrame {
            snapshot_id: 1,
            timestamp: "display".to_string(),
            display_id: 1,
            width: 100,
            height: 100,
            scale: 1.0,
            image_path: Some(PathBuf::from("/tmp/display.png")),
        };
        let window_capture = CapturedFrame {
            snapshot_id: 2,
            timestamp: "window".to_string(),
            display_id: 2,
            width: 50,
            height: 50,
            scale: 2.0,
            image_path: Some(PathBuf::from("/tmp/window.png")),
        };

        state.record_capture(
            display_capture,
            None,
            display_thumb.clone(),
            None,
            Vec::new(),
            None,
        );
        state.record_capture_without_diff_baseline(window_capture, None, None, Vec::new());

        assert_eq!(
            state.latest_thumbnail().unwrap().pixels,
            display_thumb.pixels
        );
        assert_eq!(state.latest_snapshot().unwrap().display.id, 2);
    }

    #[test]
    fn token_map_replaced_on_next_tokenization() {
        let mut state = VisionState::new();
        state.replace_token_map(vec![desktop_core::protocol::TokenEntry {
            n: 1,
            text: "Old".to_string(),
            bounds: desktop_core::protocol::Bounds {
                x: 1.0,
                y: 1.0,
                width: 10.0,
                height: 10.0,
            },
            confidence: 0.9,
        }]);
        assert!(state.token_map().get(&1).is_some());

        state.replace_token_map(vec![desktop_core::protocol::TokenEntry {
            n: 1,
            text: "New".to_string(),
            bounds: desktop_core::protocol::Bounds {
                x: 2.0,
                y: 2.0,
                width: 12.0,
                height: 12.0,
            },
            confidence: 0.8,
        }]);

        let token = state.token_map().get(&1).expect("token should exist");
        assert_eq!(token.text, "New");
    }

    #[test]
    fn tokenize_cache_requires_key_match() {
        let mut state = VisionState::new();
        state.update_tokenize_cache(
            "window:dictionary".to_string(),
            42,
            test_tokenize_payload(42),
        );

        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:other", 42)
                .is_none()
        );
        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:dictionary", 41)
                .is_none()
        );
        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:dictionary", 42)
                .is_some()
        );
    }

    #[test]
    fn tokenize_cache_hits_two_distinct_keys() {
        let mut state = VisionState::new();
        state.update_tokenize_cache("window:notes".to_string(), 10, test_tokenize_payload(10));
        state.update_tokenize_cache("window:safari".to_string(), 20, test_tokenize_payload(20));

        let notes = state
            .cached_tokenize_payload_if_fingerprint("window:notes", 10)
            .expect("notes cache hit");
        let safari = state
            .cached_tokenize_payload_if_fingerprint("window:safari", 20)
            .expect("safari cache hit");

        assert_eq!(notes.snapshot_id, 10);
        assert_eq!(safari.snapshot_id, 20);
    }

    #[test]
    fn tokenize_cache_evicts_oldest_when_capacity_exceeded() {
        let mut state = VisionState::new();
        for idx in 1..=5 {
            state.update_tokenize_cache(format!("window:{idx}"), idx, test_tokenize_payload(idx));
        }

        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:1", 1)
                .is_none()
        );
        for idx in 2..=5 {
            assert!(
                state
                    .cached_tokenize_payload_if_fingerprint(&format!("window:{idx}"), idx)
                    .is_some(),
                "window:{idx} should remain cached"
            );
        }
    }

    #[test]
    fn tokenize_cache_updates_existing_key_in_place() {
        let mut state = VisionState::new();
        state.update_tokenize_cache("window:notes".to_string(), 10, test_tokenize_payload(10));
        state.update_tokenize_cache("window:safari".to_string(), 20, test_tokenize_payload(20));
        state.update_tokenize_cache("window:notes".to_string(), 11, test_tokenize_payload(11));

        assert_eq!(state.tokenize_cache.len(), 2);
        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:notes", 10)
                .is_none()
        );
        let updated = state
            .cached_tokenize_payload_if_fingerprint("window:notes", 11)
            .expect("updated notes cache hit");
        assert_eq!(updated.snapshot_id, 11);
        assert!(
            state
                .cached_tokenize_payload_if_fingerprint("window:safari", 20)
                .is_some()
        );
    }
}
