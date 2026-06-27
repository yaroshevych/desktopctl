//! Coordinate spaces and affine transforms across capture/atspi/libei
//! (DesktopCtl-85u). Implemented by the input/coordinate agent.
//!
//! The spec ("Coordinate Model" in `tmp/ubuntu-spec.md`) defines four explicit
//! coordinate spaces and requires per-capture-stream affine transforms that
//! handle fractional scaling, mixed-DPI monitors, monitor offsets, and rotated
//! outputs. This module is intentionally dependency-free so it can be fully
//! implemented and unit-tested without portals or a graphical session.
//!
//! The four spaces (newtyped points so they cannot be mixed up accidentally):
//!
//! * [`CapturePixel`]   — raw pixel coordinates of a PipeWire capture frame.
//! * [`DesktopLogical`] — the compositor's logical/desktop coordinate space.
//! * [`AtspiScreen`]    — AT-SPI screen-relative coordinates.
//! * [`LibeiRegion`]    — coordinates within a libei device region.
//!
//! OCR returns capture-pixel coordinates and they must be converted to
//! desktop/libei space before injection.

/// A point in raw capture-frame pixel space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapturePixel {
    pub x: f64,
    pub y: f64,
}

/// A point in the compositor's logical desktop space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesktopLogical {
    pub x: f64,
    pub y: f64,
}

/// A point in AT-SPI screen-relative space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtspiScreen {
    pub x: f64,
    pub y: f64,
}

/// A point in a libei device region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LibeiRegion {
    pub x: f64,
    pub y: f64,
}

macro_rules! impl_point {
    ($($t:ty),*) => {$(
        impl $t {
            pub fn new(x: f64, y: f64) -> Self { Self { x, y } }
            fn from_xy((x, y): (f64, f64)) -> Self { Self { x, y } }
            fn xy(self) -> (f64, f64) { (self.x, self.y) }
        }
    )*};
}
impl_point!(CapturePixel, DesktopLogical, AtspiScreen, LibeiRegion);

/// Enumerates the four coordinate spaces for runtime dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    CapturePixel,
    DesktopLogical,
    AtspiScreen,
    LibeiRegion,
}

/// Output rotation, applied as part of the capture→logical mapping.
///
/// Rotation is expressed in degrees clockwise. Applying a rotation maps a
/// point on an unrotated surface of size `w x h` to its position after the
/// surface has been rotated by the given amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    /// Rotate a point `(x, y)` inside a surface of size `(w, h)` by `self`,
    /// returning the rotated coordinate. `(w, h)` is the size of the surface
    /// *before* rotation.
    pub fn apply(self, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
        match self {
            Rotation::None => (x, y),
            // Clockwise 90: new_x = h - y, new_y = x ; output extent is (h, w).
            Rotation::Cw90 => (h - y, x),
            Rotation::Cw180 => (w - x, h - y),
            // Clockwise 270: new_x = y, new_y = w - x ; output extent is (h, w).
            Rotation::Cw270 => (y, w - x),
        }
    }

    /// Inverse rotation. `(w, h)` is the size of the surface *before* rotation
    /// (same convention as [`Rotation::apply`]).
    pub fn invert(self, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
        match self {
            Rotation::None => (x, y),
            // Inverse of Cw90: rotated (rx, ry) came from (x, y) where
            // rx = h - y and ry = x, so x = ry and y = h - rx.
            Rotation::Cw90 => (y, h - x),
            Rotation::Cw180 => (w - x, h - y),
            // Inverse of Cw270: rx = y, ry = w - x, so x = w - ry, y = rx.
            Rotation::Cw270 => (w - y, x),
        }
    }

    pub fn degrees(self) -> u32 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 90,
            Rotation::Cw180 => 180,
            Rotation::Cw270 => 270,
        }
    }

    pub fn from_degrees(d: u32) -> Self {
        match d % 360 {
            90 => Rotation::Cw90,
            180 => Rotation::Cw180,
            270 => Rotation::Cw270,
            _ => Rotation::None,
        }
    }
}

/// A 2D affine transform of the form:
///
/// ```text
/// x' = sx * x + tx
/// y' = sy * y + ty
/// ```
///
/// This is an axis-aligned scale + translation, which is sufficient for the
/// fractional-scaling / mixed-DPI / monitor-offset cases in the spec. Rotation
/// is handled separately by [`Rotation`] because it changes axis extents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffineTransform {
    pub sx: f64,
    pub sy: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Default for AffineTransform {
    fn default() -> Self {
        Self::identity()
    }
}

impl AffineTransform {
    pub fn identity() -> Self {
        Self {
            sx: 1.0,
            sy: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// A uniform scale with no translation (fractional scale supported).
    pub fn scale(s: f64) -> Self {
        Self {
            sx: s,
            sy: s,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// A per-axis scale with no translation (mixed-DPI / non-uniform scaling).
    pub fn scale_xy(sx: f64, sy: f64) -> Self {
        Self {
            sx,
            sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// A pure translation (monitor offsets).
    pub fn translate(tx: f64, ty: f64) -> Self {
        Self {
            sx: 1.0,
            sy: 1.0,
            tx,
            ty,
        }
    }

    /// A scale followed by an offset: `x' = s*x + offset`.
    pub fn scale_then_offset(sx: f64, sy: f64, tx: f64, ty: f64) -> Self {
        Self { sx, sy, tx, ty }
    }

    /// Apply the transform to a point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.sx * x + self.tx, self.sy * y + self.ty)
    }

    /// Invert the transform. Returns `None` if either scale factor is zero
    /// (non-invertible, e.g. a degenerate/uninitialized transform).
    pub fn invert(&self) -> Option<AffineTransform> {
        if self.sx == 0.0 || self.sy == 0.0 {
            return None;
        }
        let isx = 1.0 / self.sx;
        let isy = 1.0 / self.sy;
        Some(AffineTransform {
            sx: isx,
            sy: isy,
            tx: -self.tx * isx,
            ty: -self.ty * isy,
        })
    }

    /// Compose two transforms so the result applies `self` first, then `other`:
    /// `compose(self, other).apply(p) == other.apply(self.apply(p))`.
    pub fn compose(&self, other: &AffineTransform) -> AffineTransform {
        AffineTransform {
            sx: other.sx * self.sx,
            sy: other.sy * self.sy,
            tx: other.sx * self.tx + other.tx,
            ty: other.sy * self.ty + other.ty,
        }
    }
}

/// Per-capture-stream coordinate model.
///
/// Holds the affine transform that maps capture-pixel coordinates into desktop
/// logical space (incorporating fractional scale and monitor offset), the
/// output rotation, the capture frame size (needed to apply rotation), the
/// libei device region origin within desktop space, and the AT-SPI screen
/// origin. Methods convert points (and bounds) between any two spaces.
///
/// Transforms become stale after a display / scale / rotation / stream change
/// and must be rebuilt; [`CoordinateMap::invalidate`] marks the map so callers
/// can detect and refuse to map against a stale model (the spec requires
/// invalidation on such changes).
#[derive(Debug, Clone)]
pub struct CoordinateMap {
    /// capture-pixel → desktop-logical scale+offset (excludes rotation).
    capture_to_logical: AffineTransform,
    /// Output rotation applied between capture pixels and logical space.
    rotation: Rotation,
    /// Capture frame size in pixels (pre-rotation), used by rotation math.
    capture_size: (f64, f64),
    /// libei device region origin within desktop-logical space.
    libei_origin: (f64, f64),
    /// AT-SPI screen origin within desktop-logical space (usually 0,0; the
    /// global virtual screen). Scale is assumed 1:1 (AT-SPI reports logical px).
    atspi_origin: (f64, f64),
    /// Set to true once the underlying display configuration changes; the map
    /// must be rebuilt before being trusted again.
    invalidated: bool,
}

impl CoordinateMap {
    /// Build a map from the per-stream pieces.
    ///
    /// * `capture_to_logical` — scale+offset taking capture pixels (after
    ///   rotation) to desktop-logical coordinates.
    /// * `rotation` — output rotation.
    /// * `capture_size` — pixel size of the capture frame before rotation.
    /// * `libei_origin` — origin of the libei device region in logical space.
    /// * `atspi_origin` — origin of the AT-SPI screen in logical space.
    pub fn new(
        capture_to_logical: AffineTransform,
        rotation: Rotation,
        capture_size: (f64, f64),
        libei_origin: (f64, f64),
        atspi_origin: (f64, f64),
    ) -> Self {
        Self {
            capture_to_logical,
            rotation,
            capture_size,
            libei_origin,
            atspi_origin,
            invalidated: false,
        }
    }

    /// A trivial identity map (1:1 scale, no rotation/offset). Useful as a
    /// starting point and for tests.
    pub fn identity(capture_size: (f64, f64)) -> Self {
        Self::new(
            AffineTransform::identity(),
            Rotation::None,
            capture_size,
            (0.0, 0.0),
            (0.0, 0.0),
        )
    }

    /// Mark the map stale after a display/scale/rotation/stream change.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// Whether the map has been invalidated and needs rebuilding.
    pub fn is_invalidated(&self) -> bool {
        self.invalidated
    }

    // --- capture <-> logical ------------------------------------------------

    fn capture_to_logical_xy(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = self.capture_size;
        let (rx, ry) = self.rotation.apply(x, y, w, h);
        self.capture_to_logical.apply(rx, ry)
    }

    fn logical_to_capture_xy(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let inv = self.capture_to_logical.invert()?;
        let (ux, uy) = inv.apply(x, y);
        let (w, h) = self.capture_size;
        Some(self.rotation.invert(ux, uy, w, h))
    }

    // --- public point conversions ------------------------------------------

    pub fn capture_to_desktop(&self, p: CapturePixel) -> DesktopLogical {
        let (x, y) = p.xy();
        DesktopLogical::from_xy(self.capture_to_logical_xy(x, y))
    }

    pub fn desktop_to_capture(&self, p: DesktopLogical) -> Option<CapturePixel> {
        let (x, y) = p.xy();
        self.logical_to_capture_xy(x, y).map(CapturePixel::from_xy)
    }

    pub fn desktop_to_libei(&self, p: DesktopLogical) -> LibeiRegion {
        LibeiRegion::new(p.x - self.libei_origin.0, p.y - self.libei_origin.1)
    }

    pub fn libei_to_desktop(&self, p: LibeiRegion) -> DesktopLogical {
        DesktopLogical::new(p.x + self.libei_origin.0, p.y + self.libei_origin.1)
    }

    pub fn desktop_to_atspi(&self, p: DesktopLogical) -> AtspiScreen {
        AtspiScreen::new(p.x - self.atspi_origin.0, p.y - self.atspi_origin.1)
    }

    pub fn atspi_to_desktop(&self, p: AtspiScreen) -> DesktopLogical {
        DesktopLogical::new(p.x + self.atspi_origin.0, p.y + self.atspi_origin.1)
    }

    // --- composed convenience conversions -----------------------------------

    pub fn capture_to_libei(&self, p: CapturePixel) -> LibeiRegion {
        self.desktop_to_libei(self.capture_to_desktop(p))
    }

    pub fn capture_to_atspi(&self, p: CapturePixel) -> AtspiScreen {
        self.desktop_to_atspi(self.capture_to_desktop(p))
    }

    pub fn atspi_to_libei(&self, p: AtspiScreen) -> LibeiRegion {
        self.desktop_to_libei(self.atspi_to_desktop(p))
    }

    // --- generic dispatch via (x, y) ----------------------------------------

    /// Convert a raw `(x, y)` pair from one space to another. Returns `None`
    /// only when an inverse is required but the transform is non-invertible.
    pub fn convert(&self, from: Space, to: Space, x: f64, y: f64) -> Option<(f64, f64)> {
        // First lift the source into desktop-logical space.
        let logical = match from {
            Space::DesktopLogical => DesktopLogical::new(x, y),
            Space::CapturePixel => self.capture_to_desktop(CapturePixel::new(x, y)),
            Space::AtspiScreen => self.atspi_to_desktop(AtspiScreen::new(x, y)),
            Space::LibeiRegion => self.libei_to_desktop(LibeiRegion::new(x, y)),
        };
        // Then project into the destination space.
        let out = match to {
            Space::DesktopLogical => logical.xy(),
            Space::CapturePixel => self.desktop_to_capture(logical)?.xy(),
            Space::AtspiScreen => self.desktop_to_atspi(logical).xy(),
            Space::LibeiRegion => self.desktop_to_libei(logical).xy(),
        };
        Some(out)
    }

    /// Convert an axis-aligned bounds rectangle between two spaces. Because the
    /// supported transforms are axis-aligned scale+offset (plus 90° rotations),
    /// the result is obtained by converting two opposite corners and
    /// normalizing. Returns `(x, y, w, h)` in the destination space.
    pub fn convert_bounds(
        &self,
        from: Space,
        to: Space,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        let (ax, ay) = self.convert(from, to, x, y)?;
        let (bx, by) = self.convert(from, to, x + w, y + h)?;
        let nx = ax.min(bx);
        let ny = ay.min(by);
        let nw = (ax - bx).abs();
        let nh = (ay - by).abs();
        Some((nx, ny, nw, nh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ≈ {b}");
    }

    fn approx_pt(a: (f64, f64), b: (f64, f64)) {
        approx(a.0, b.0);
        approx(a.1, b.1);
    }

    #[test]
    fn affine_apply_and_invert_roundtrip() {
        let t = AffineTransform::scale_then_offset(1.5, 2.0, 10.0, -5.0);
        let (x, y) = t.apply(8.0, 3.0);
        approx_pt((x, y), (1.5 * 8.0 + 10.0, 2.0 * 3.0 - 5.0));
        let inv = t.invert().unwrap();
        approx_pt(inv.apply(x, y), (8.0, 3.0));
    }

    #[test]
    fn affine_invert_rejects_zero_scale() {
        assert!(AffineTransform::scale_xy(0.0, 1.0).invert().is_none());
        assert!(AffineTransform::scale_xy(1.0, 0.0).invert().is_none());
    }

    #[test]
    fn affine_compose_order() {
        // scale by 2 then translate by (3, 4).
        let s = AffineTransform::scale(2.0);
        let tr = AffineTransform::translate(3.0, 4.0);
        let c = s.compose(&tr);
        // apply(p) should equal tr.apply(s.apply(p)).
        let p = (5.0, 6.0);
        let mid = s.apply(p.0, p.1);
        let expected = tr.apply(mid.0, mid.1);
        approx_pt(c.apply(p.0, p.1), expected);
        approx_pt(c.apply(p.0, p.1), (2.0 * 5.0 + 3.0, 2.0 * 6.0 + 4.0));
    }

    #[test]
    fn fractional_scale_roundtrip() {
        // 1.25 fractional scale, 1920x1080 capture, monitor offset (200, 0).
        let map = CoordinateMap::new(
            AffineTransform::scale_then_offset(1.0 / 1.25, 1.0 / 1.25, 200.0, 0.0),
            Rotation::None,
            (1920.0, 1080.0),
            (0.0, 0.0),
            (0.0, 0.0),
        );
        let cap = CapturePixel::new(640.0, 360.0);
        let desk = map.capture_to_desktop(cap);
        approx_pt(desk.xy(), (640.0 / 1.25 + 200.0, 360.0 / 1.25));
        let back = map.desktop_to_capture(desk).unwrap();
        approx_pt(back.xy(), cap.xy());
    }

    #[test]
    fn mixed_dpi_non_uniform_scale_roundtrip() {
        let map = CoordinateMap::new(
            AffineTransform::scale_then_offset(0.5, 0.75, -100.0, 50.0),
            Rotation::None,
            (3840.0, 2160.0),
            (0.0, 0.0),
            (0.0, 0.0),
        );
        let cap = CapturePixel::new(1000.0, 800.0);
        let desk = map.capture_to_desktop(cap);
        let back = map.desktop_to_capture(desk).unwrap();
        approx_pt(back.xy(), cap.xy());
    }

    #[test]
    fn rotation_roundtrip_all_angles() {
        for rot in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Cw180,
            Rotation::Cw270,
        ] {
            let map = CoordinateMap::new(
                AffineTransform::scale(2.0),
                rot,
                (1920.0, 1080.0),
                (0.0, 0.0),
                (0.0, 0.0),
            );
            let cap = CapturePixel::new(300.0, 700.0);
            let desk = map.capture_to_desktop(cap);
            let back = map.desktop_to_capture(desk).unwrap();
            approx_pt(back.xy(), cap.xy());
        }
    }

    #[test]
    fn rotation_apply_invert_consistency() {
        let (w, h) = (1920.0, 1080.0);
        for rot in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Cw180,
            Rotation::Cw270,
        ] {
            let (rx, ry) = rot.apply(123.0, 456.0, w, h);
            approx_pt(rot.invert(rx, ry, w, h), (123.0, 456.0));
        }
    }

    #[test]
    fn libei_and_atspi_offsets() {
        let map = CoordinateMap::new(
            AffineTransform::identity(),
            Rotation::None,
            (1920.0, 1080.0),
            (1920.0, 0.0), // second monitor to the right
            (0.0, 0.0),
        );
        let desk = DesktopLogical::new(2000.0, 100.0);
        let ei = map.desktop_to_libei(desk);
        approx_pt(ei.xy(), (80.0, 100.0));
        approx_pt(map.libei_to_desktop(ei).xy(), desk.xy());
        let at = map.desktop_to_atspi(desk);
        approx_pt(map.atspi_to_desktop(at).xy(), desk.xy());
    }

    #[test]
    fn convert_dispatch_roundtrip() {
        let map = CoordinateMap::new(
            AffineTransform::scale_then_offset(0.8, 0.8, 50.0, 25.0),
            Rotation::Cw90,
            (1280.0, 720.0),
            (10.0, 20.0),
            (0.0, 0.0),
        );
        // capture -> libei -> capture
        let (lx, ly) = map
            .convert(Space::CapturePixel, Space::LibeiRegion, 400.0, 200.0)
            .unwrap();
        let (cx, cy) = map
            .convert(Space::LibeiRegion, Space::CapturePixel, lx, ly)
            .unwrap();
        approx_pt((cx, cy), (400.0, 200.0));
    }

    #[test]
    fn convert_bounds_roundtrip() {
        let map = CoordinateMap::new(
            AffineTransform::scale_then_offset(0.5, 0.5, 0.0, 0.0),
            Rotation::None,
            (1920.0, 1080.0),
            (0.0, 0.0),
            (0.0, 0.0),
        );
        let (x, y, w, h) = map
            .convert_bounds(
                Space::CapturePixel,
                Space::DesktopLogical,
                100.0,
                200.0,
                40.0,
                60.0,
            )
            .unwrap();
        approx_pt((x, y), (50.0, 100.0));
        approx_pt((w, h), (20.0, 30.0));
        let (bx, by, bw, bh) = map
            .convert_bounds(Space::DesktopLogical, Space::CapturePixel, x, y, w, h)
            .unwrap();
        approx_pt((bx, by), (100.0, 200.0));
        approx_pt((bw, bh), (40.0, 60.0));
    }

    #[test]
    fn invalidate_marker() {
        let mut map = CoordinateMap::identity((800.0, 600.0));
        assert!(!map.is_invalidated());
        map.invalidate();
        assert!(map.is_invalidated());
    }
}
