use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use desktop_core::{
    error::AppError,
    protocol::{Bounds, SnapshotDisplay, SnapshotPayload, TokenEntry},
};

use super::{diff::GrayThumbnail, types::CapturedFrame};

const MAX_EVENTS: usize = 512;
const MAX_FRAMES: usize = 64;

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
    latest_thumbnail: Option<GrayThumbnail>,
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
            latest_thumbnail: None,
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

    pub fn token_map(&self) -> &HashMap<u32, TokenEntry> {
        &self.token_map
    }

    pub fn token(&self, n: u32) -> Option<TokenEntry> {
        self.token_map.get(&n).cloned()
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
        thumbnail: GrayThumbnail,
        focused_app: Option<String>,
        texts: Vec<desktop_core::protocol::SnapshotText>,
        _roi: Option<Bounds>,
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
        self.latest_frame_path = Some(capture.image_path.clone());
        self.latest_thumbnail = Some(thumbnail);
        self.frames.push_back(capture.image_path);
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
            image_path: PathBuf::from("/tmp/a.png"),
        };
        let capture2 = CapturedFrame {
            snapshot_id: 2,
            timestamp: "t2".to_string(),
            display_id: 1,
            width: 100,
            height: 100,
            scale: 2.0,
            image_path: PathBuf::from("/tmp/b.png"),
        };
        let first = state.record_capture(capture1, thumb.clone(), None, Vec::new(), None);
        let second = state.record_capture(capture2, thumb, None, Vec::new(), None);
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
            image_path: PathBuf::from("/tmp/a.png"),
        };
        let first =
            state.record_capture(base_capture.clone(), thumb.clone(), None, Vec::new(), None);
        let second = state.record_capture(base_capture, thumb, None, Vec::new(), None);
        assert!(first.event_id < second.event_id);
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
        assert!(state.token(1).is_some());

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

        let token = state.token(1).expect("token should exist");
        assert_eq!(token.text, "New");
    }
}
