use desktop_core::{error::AppError, protocol::Bounds};

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
}

#[cfg(target_os = "macos")]
mod macos {
    use accessibility::{
        AXAttribute, AXUIElement, AXUIElementAttributes, TreeVisitor, TreeWalker, TreeWalkerFlow,
    };
    use accessibility_sys::{
        AXValueGetType, AXValueGetValue, AXValueRef, kAXFocusedApplicationAttribute,
        kAXPositionAttribute, kAXSizeAttribute, kAXValueTypeCGPoint, kAXValueTypeCGSize,
    };
    use core_foundation::{
        base::{CFType, TCFType},
        string::CFString,
    };
    use desktop_core::{error::AppError, protocol::Bounds};
    use std::{cell::RefCell, ffi::c_void};

    use super::AxElement;

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    pub(super) fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
        let system = AXUIElement::system_wide();
        let focused_app_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
            kAXFocusedApplicationAttribute,
        ));
        let app_cf = system.attribute(&focused_app_attr).map_err(ax_err)?;
        if !app_cf.instance_of::<AXUIElement>() {
            return Err(AppError::backend_unavailable(
                "focused application AX value had unexpected type",
            ));
        }
        let app = unsafe { AXUIElement::wrap_under_get_rule(app_cf.as_CFTypeRef() as _) };
        let window = app
            .focused_window()
            .or_else(|_| app.main_window())
            .map_err(ax_err)?;

        let visitor = Collector::default();
        TreeWalker::new().walk(&window, &visitor);
        Ok(visitor.into_elements())
    }

    #[derive(Default)]
    struct Collector {
        elements: RefCell<Vec<AxElement>>,
    }

    impl Collector {
        fn into_elements(self) -> Vec<AxElement> {
            self.elements.into_inner()
        }
    }

    impl TreeVisitor for Collector {
        fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
            let Some(role) = element.role().ok().map(|v| v.to_string()) else {
                return TreeWalkerFlow::Continue;
            };

            if !is_interactive_role(&role) {
                return TreeWalkerFlow::Continue;
            }

            let Some(bounds) = element_bounds(element) else {
                return TreeWalkerFlow::Continue;
            };

            let text = element_label(element, &role);

            self.elements
                .borrow_mut()
                .push(AxElement { role, text, bounds });
            TreeWalkerFlow::Continue
        }

        fn exit_element(&self, _element: &AXUIElement) {}
    }

    fn is_interactive_role(role: &str) -> bool {
        matches!(
            role,
            "AXButton"
                | "AXCheckBox"
                | "AXRadioButton"
                | "AXPopUpButton"
                | "AXTextField"
                | "AXComboBox"
                | "AXSlider"
                | "AXMenuButton"
                | "AXScrollBar"
                | "AXScrollArea"
                | "AXValueIndicator"
                | "AXIncrementor"
                | "AXSplitter"
        )
    }

    fn element_label(element: &AXUIElement, role: &str) -> Option<String> {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(v) = element.title() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.description() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.label_value() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.value_description() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.placeholder_value() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.help() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.attribute(&AXAttribute::value()) {
            if v.instance_of::<CFString>() {
                candidates.push(
                    unsafe { CFString::wrap_under_get_rule(v.as_CFTypeRef() as _) }.to_string(),
                );
            }
        }

        let primary = candidates
            .into_iter()
            .map(|s| s.trim().to_string())
            .find(|s| !s.is_empty());
        if primary.is_some() {
            return primary;
        }

        // Some controls (notably calculator-style buttons) expose their visible
        // label in child static text instead of the button's own title/value.
        if role == "AXButton" || role == "AXPopUpButton" || role == "AXMenuButton" {
            return element.children().ok().and_then(|children| {
                children.into_iter().find_map(|child| {
                    child
                        .title()
                        .ok()
                        .map(|v| v.to_string())
                        .or_else(|| {
                            child.attribute(&AXAttribute::value()).ok().and_then(|v| {
                                if v.instance_of::<CFString>() {
                                    Some(
                                        unsafe {
                                            CFString::wrap_under_get_rule(v.as_CFTypeRef() as _)
                                        }
                                        .to_string(),
                                    )
                                } else {
                                    None
                                }
                            })
                        })
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
            });
        }
        None
    }

    fn element_bounds(element: &AXUIElement) -> Option<Bounds> {
        let pos_attr =
            AXAttribute::<CFType>::new(&CFString::from_static_string(kAXPositionAttribute));
        let size_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXSizeAttribute));
        let pos = element.attribute(&pos_attr).ok()?;
        let size = element.attribute(&size_attr).ok()?;

        let (x, y) = decode_point(&pos)?;
        let (w, h) = decode_size(&size)?;
        if w <= 1.0 || h <= 1.0 {
            return None;
        }
        Some(Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: w.max(0.0),
            height: h.max(0.0),
        })
    }

    fn decode_point(value: &CFType) -> Option<(f64, f64)> {
        let mut point = CGPoint { x: 0.0, y: 0.0 };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGPoint
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGPoint,
                    (&mut point as *mut CGPoint).cast::<c_void>(),
                )
        };
        ok.then_some((point.x, point.y))
    }

    fn decode_size(value: &CFType) -> Option<(f64, f64)> {
        let mut size = CGSize {
            width: 0.0,
            height: 0.0,
        };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGSize
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGSize,
                    (&mut size as *mut CGSize).cast::<c_void>(),
                )
        };
        ok.then_some((size.width, size.height))
    }

    fn ax_err(err: accessibility::Error) -> AppError {
        AppError::backend_unavailable(format!("failed to query AX tree: {err}"))
    }
}

#[cfg(target_os = "macos")]
pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    macos::collect_frontmost_window_elements()
}

#[cfg(not(target_os = "macos"))]
pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    Ok(Vec::new())
}
