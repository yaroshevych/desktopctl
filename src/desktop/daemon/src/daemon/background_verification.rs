use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use desktop_core::{automation::BackgroundInputTarget, error::AppError, protocol::Bounds};
use image::RgbaImage;
use serde_json::{Value, json};

use crate::{
    trace,
    vision::{self, pipeline::TokenizeWindowMeta},
};

const DEFAULT_SETTLE_MS: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundInputVerificationStatus {
    Success,
    EffectNotVerified,
    Ambiguous,
}

impl BackgroundInputVerificationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::EffectNotVerified => "effect_not_verified",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundInputVerification {
    status: BackgroundInputVerificationStatus,
    reason: String,
    settle_ms: u64,
}

impl BackgroundInputVerification {
    fn new(status: BackgroundInputVerificationStatus, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            settle_ms: settle_ms(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "reason": self.reason,
            "settle_ms": self.settle_ms
        })
    }
}

pub(crate) fn enabled() -> bool {
    std::env::var("DESKTOPCTL_BACKGROUND_VERIFY")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

pub(crate) fn append(result: &mut Value, verification: Option<BackgroundInputVerification>) {
    if let (Some(obj), Some(verification)) = (result.as_object_mut(), verification) {
        obj.insert(
            "background_verification".to_string(),
            verification.to_json(),
        );
    }
}

pub(crate) fn verify_after_action(
    action: &str,
    target: &BackgroundInputTarget,
    action_fn: impl FnOnce() -> Result<(), AppError>,
) -> Result<Option<BackgroundInputVerification>, AppError> {
    if !enabled() {
        action_fn()?;
        return Ok(None);
    }

    trace::log(format!(
        "background_input:verify_start action={} pid={} window_id={}",
        action, target.pid, target.window_id
    ));
    let before = VerificationSnapshot::capture(target);
    let dispatch = action_fn();
    if let Err(err) = dispatch {
        trace::log(format!(
            "background_input:verify_transport_failed action={} pid={} window_id={} err={}",
            action, target.pid, target.window_id, err
        ));
        return Err(err);
    }

    std::thread::sleep(Duration::from_millis(settle_ms()));
    let after = VerificationSnapshot::capture(target);
    let verification = classify(action, before, after);
    trace::log(format!(
        "background_input:verify_done action={} pid={} window_id={} status={} reason={}",
        action,
        target.pid,
        target.window_id,
        verification.status.as_str(),
        verification.reason
    ));
    Ok(Some(verification))
}

fn classify(
    action: &str,
    before: VerificationSnapshot,
    after: VerificationSnapshot,
) -> BackgroundInputVerification {
    if let (Some(before), Some(after)) = (before.semantic.as_ref(), after.semantic.as_ref()) {
        if before != after {
            return BackgroundInputVerification::new(
                BackgroundInputVerificationStatus::Success,
                format!("{action} changed target window token/AX content"),
            );
        }
    }

    if let (Ok(before), Ok(after)) = (before.capture.as_ref(), after.capture.as_ref())
        && before != after
    {
        return BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::Success,
            format!("{action} changed the target window capture"),
        );
    }

    if before.semantic.is_some() && after.semantic.is_some() {
        return BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::EffectNotVerified,
            format!("{action} produced no target window token/AX or capture delta"),
        );
    }

    classify_capture_only(action, before.capture, after.capture)
}

fn classify_capture_only(
    action: &str,
    before: Result<CaptureFingerprint, AppError>,
    after: Result<CaptureFingerprint, AppError>,
) -> BackgroundInputVerification {
    match (before, after) {
        (Ok(before), Ok(after)) if before != after => BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::Success,
            format!("{action} changed the target window capture"),
        ),
        (Ok(_), Ok(_)) => BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::EffectNotVerified,
            format!("{action} produced no target window capture delta"),
        ),
        (Err(before), Ok(_)) => BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::Ambiguous,
            format!(
                "pre-action target window capture failed: {}",
                before.message
            ),
        ),
        (Ok(_), Err(after)) => BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::Ambiguous,
            format!(
                "post-action target window capture failed: {}",
                after.message
            ),
        ),
        (Err(before), Err(after)) => BackgroundInputVerification::new(
            BackgroundInputVerificationStatus::Ambiguous,
            format!(
                "target window capture failed before and after action: {}; {}",
                before.message, after.message
            ),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaptureFingerprint {
    width: u32,
    height: u32,
    hash: u64,
}

#[derive(Debug)]
struct VerificationSnapshot {
    capture: Result<CaptureFingerprint, AppError>,
    semantic: Option<SemanticFingerprint>,
}

impl VerificationSnapshot {
    fn capture(target: &BackgroundInputTarget) -> Self {
        let capture = capture_fingerprint(target);
        let semantic = match semantic_fingerprint(target) {
            Ok(snapshot) => Some(snapshot),
            Err(err) => {
                trace::log(format!(
                    "background_input:verify_semantic_warn pid={} window_id={} err={}",
                    target.pid, target.window_id, err
                ));
                None
            }
        };
        Self { capture, semantic }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticFingerprint {
    elements: Vec<String>,
}

fn target_bounds(target: &BackgroundInputTarget) -> Bounds {
    Bounds {
        x: target.bounds.x,
        y: target.bounds.y,
        width: target.bounds.width,
        height: target.bounds.height,
    }
}

fn capture_fingerprint(target: &BackgroundInputTarget) -> Result<CaptureFingerprint, AppError> {
    let captured =
        vision::capture::capture_window_png(None, target.window_id, Some(&target_bounds(target)))?;
    Ok(fingerprint_image(&captured.image))
}

fn semantic_fingerprint(target: &BackgroundInputTarget) -> Result<SemanticFingerprint, AppError> {
    let bounds = target_bounds(target);
    let payload = vision::pipeline::tokenize_window(TokenizeWindowMeta {
        id: target.window_id.to_string(),
        title: String::new(),
        app: None,
        bounds: bounds.clone(),
        pid: Some(target.pid),
        native_window_id: Some(target.window_id),
        capture_bounds: Some(bounds),
    })?;
    let mut elements = Vec::new();
    for window in payload.windows {
        for element in window.elements {
            elements.push(format!(
                "{}|{}|{}|{}|{}|{:.1},{:.1},{:.1},{:.1}",
                element.source,
                element.kind,
                element.text.unwrap_or_default(),
                element
                    .checked
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_default(),
                element.scrollable.unwrap_or(false),
                element.bbox[0],
                element.bbox[1],
                element.bbox[2],
                element.bbox[3]
            ));
        }
    }
    elements.sort();
    Ok(SemanticFingerprint { elements })
}

fn fingerprint_image(image: &RgbaImage) -> CaptureFingerprint {
    let mut hasher = DefaultHasher::new();
    image.width().hash(&mut hasher);
    image.height().hash(&mut hasher);
    image.as_raw().hash(&mut hasher);
    CaptureFingerprint {
        width: image.width(),
        height: image.height(),
        hash: hasher.finish(),
    }
}

fn settle_ms() -> u64 {
    std::env::var("DESKTOPCTL_BACKGROUND_VERIFY_SETTLE_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SETTLE_MS)
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundInputVerification, BackgroundInputVerificationStatus, CaptureFingerprint,
        SemanticFingerprint, VerificationSnapshot, classify,
    };
    use desktop_core::error::AppError;

    #[test]
    fn classify_success_when_capture_changes() {
        let result = classify("click", snapshot_with_capture(1), snapshot_with_capture(2));
        assert_eq!(result.status, BackgroundInputVerificationStatus::Success);
    }

    #[test]
    fn classify_success_when_semantic_snapshot_changes() {
        let result = classify(
            "type_text",
            VerificationSnapshot {
                capture: Ok(CaptureFingerprint {
                    width: 2,
                    height: 2,
                    hash: 1,
                }),
                semantic: Some(SemanticFingerprint {
                    elements: vec!["text|before".to_string()],
                }),
            },
            VerificationSnapshot {
                capture: Ok(CaptureFingerprint {
                    width: 2,
                    height: 2,
                    hash: 1,
                }),
                semantic: Some(SemanticFingerprint {
                    elements: vec!["text|after".to_string()],
                }),
            },
        );
        assert_eq!(result.status, BackgroundInputVerificationStatus::Success);
    }

    #[test]
    fn classify_unverified_when_capture_is_identical() {
        let result = classify(
            "type_text",
            snapshot_with_capture(1),
            snapshot_with_capture(1),
        );
        assert_eq!(
            result.status,
            BackgroundInputVerificationStatus::EffectNotVerified
        );
    }

    #[test]
    fn classify_ambiguous_when_capture_fails() {
        let result = classify(
            "scroll",
            VerificationSnapshot {
                capture: Err(AppError::backend_unavailable("capture failed")),
                semantic: None,
            },
            snapshot_with_capture(1),
        );
        assert_eq!(result.status, BackgroundInputVerificationStatus::Ambiguous);
    }

    fn snapshot_with_capture(hash: u64) -> VerificationSnapshot {
        VerificationSnapshot {
            capture: Ok(CaptureFingerprint {
                width: 2,
                height: 2,
                hash,
            }),
            semantic: None,
        }
    }
}
