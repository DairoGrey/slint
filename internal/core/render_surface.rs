// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! A small, renderer-backed display-list surface for host integrations.
//!
//! Render surfaces deliberately are not item trees. A producer publishes an
//! immutable bounded frame and Slint renders it inside one clipped item.

use crate::SharedString;
use crate::graphics::{Brush, Color, FontRequest};
use crate::item_rendering::{
    HasFont, PlainOrStyledText, RenderRectangle, RenderString, RenderText,
};
use crate::items::{
    TextHorizontalAlignment, TextOverflow, TextStrokeStyle, TextVerticalAlignment, TextWrap,
};
use crate::lengths::LogicalLength;
use crate::thread_local;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_void;
use core::ops::Range;

/// One immutable display list consumed by a [`crate::items::RenderSurfaceItem`].
#[derive(Clone, Default)]
pub struct RenderSurfaceFrame {
    /// Monotonically increasing producer generation. Renderers do not attach
    /// semantics to it, but it is useful for diagnostics and tests.
    pub generation: u64,
    /// Generation of immutable content commands.
    pub base_generation: u64,
    pub underlay_generation: u64,
    /// Generation of transient overlay commands.
    pub overlay_generation: u64,
    /// Commands are positioned in the local coordinate system of the item.
    pub commands: Rc<Vec<RenderSurfaceCommand>>,
    pub underlay_commands: Rc<Vec<RenderSurfaceCommand>>,
    /// Commands drawn after `commands`, for carets, selection and other
    /// transient overlays.
    pub overlay_commands: Rc<Vec<RenderSurfaceCommand>>,
}

/// A set of independently replaceable render-surface display-list layers.
///
/// A delta deliberately distinguishes an omitted layer from an explicitly
/// empty one: omitted layers retain their existing immutable list, while an
/// included empty layer clears that list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSurfaceLayerMask(u8);

impl RenderSurfaceLayerMask {
    pub const BASE: Self = Self(1);
    pub const UNDERLAY: Self = Self(2);
    pub const OVERLAY: Self = Self(4);
    pub const ALL: Self = Self(Self::BASE.0 | Self::UNDERLAY.0 | Self::OVERLAY.0);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }
    pub const fn contains(self, layer: Self) -> bool {
        self.0 & layer.0 != 0
    }
}

impl core::ops::BitOr for RenderSurfaceLayerMask {
    type Output = Self;

    fn bitor(self, right: Self) -> Self::Output {
        Self::from_bits(self.0 | right.0)
    }
}

/// A primitive command accepted by render-surface renderers.
#[derive(Clone)]
pub enum RenderSurfaceCommand {
    /// A solid filled rectangle.
    FillRect { x: f32, y: f32, width: f32, height: f32, color: Color },
    /// A text run with an explicit font request and local origin.
    Text {
        /// Stable host-assigned key used to correlate post-shaping geometry
        /// with this exact text command. Zero opts out of layout reporting.
        layout_key: u64,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        text: SharedString,
        color: Color,
        /// Optional foreground-colour overrides for byte ranges in `text`.
        spans: Vec<RenderSurfaceTextSpan>,
        font: FontRequest,
        horizontal_alignment: TextHorizontalAlignment,
        vertical_alignment: TextVerticalAlignment,
    },
    /// A horizontal or vertical solid line. Arbitrary angled paths are outside
    /// this intentionally small display-list contract.
    Line { x: f32, y: f32, width: f32, height: f32, color: Color },
}

/// One shaped text cluster in a render-surface text command. Coordinates are
/// local logical coordinates relative to the command's origin.
#[derive(Clone, Copy, Default)]
pub struct RenderSurfaceLayoutCluster {
    pub start_byte: u32,
    pub end_byte: u32,
    pub x: f32,
    pub width: f32,
}

/// Immutable post-shaping geometry for one text command. This is deliberately
/// renderer-neutral: hosts receive logical cluster positions, never renderer
/// objects or glyph cache handles.
#[derive(Clone, Default)]
pub struct RenderSurfaceLayoutSnapshot {
    pub layout_key: u64,
    pub baseline: f32,
    pub advance: f32,
    pub clusters: Vec<RenderSurfaceLayoutCluster>,
}

/// A foreground-colour override within a UTF-8 text command.
#[derive(Clone)]
pub struct RenderSurfaceTextSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub color: Color,
}

thread_local! {
    static SURFACES: RefCell<BTreeMap<i32, Rc<RenderSurfaceFrame>>> = Default::default();
    // The callback is intentionally UI-thread-local, just like the surface
    // registry. It is a diagnostic lifecycle hook for hosts; renderers do not
    // retain host state or depend on a callback being installed.
    static PROCESSED_CALLBACK: RefCell<Option<RenderSurfaceProcessedCallback>> = Default::default();
    static DRAW_STARTED_CALLBACK: RefCell<Option<RenderSurfaceDrawStartedCallback>> = Default::default();
    static LAYOUT_BATCH_CALLBACK: RefCell<Option<RenderSurfaceLayoutBatchCallback>> = Default::default();
}

#[derive(Clone, Copy)]
pub struct RenderSurfaceProcessedCallback {
    pub callback: unsafe extern "C" fn(i32, u64, *mut c_void),
    pub user_data: *mut c_void,
}

#[derive(Clone, Copy)]
pub struct RenderSurfaceDrawStartedCallback {
    pub callback: unsafe extern "C" fn(i32, u64, usize, usize, usize, *mut c_void),
    pub user_data: *mut c_void,
}

#[derive(Clone, Copy)]
pub struct RenderSurfaceLayoutBatchCallback {
    pub callback:
        unsafe extern "C" fn(i32, u64, *const RenderSurfaceLayoutSnapshot, usize, *mut c_void),
    pub user_data: *mut c_void,
}

/// Installs (or clears) the callback emitted after the active backend has
/// processed a render-surface frame. A fully clipped frame is processed too;
/// this is deliberately not an OS-compositor presentation notification.
pub fn set_render_surface_processed_callback(callback: Option<RenderSurfaceProcessedCallback>) {
    PROCESSED_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

pub fn set_render_surface_draw_started_callback(
    callback: Option<RenderSurfaceDrawStartedCallback>,
) {
    DRAW_STARTED_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

#[allow(unsafe_code)] // FFI callback is installed only by the public C++ bridge.
pub fn notify_render_surface_draw_started(
    surface_id: i32,
    generation: u64,
    base: usize,
    underlay: usize,
    overlay: usize,
) {
    DRAW_STARTED_CALLBACK.with(|slot| {
        if let Some(callback) = *slot.borrow() {
            unsafe {
                (callback.callback)(
                    surface_id,
                    generation,
                    base,
                    underlay,
                    overlay,
                    callback.user_data,
                )
            };
        }
    });
}

/// Installs the UI-thread callback that receives geometry from the same Parley
/// layout used to render each render-surface text command.
pub fn set_render_surface_layout_batch_callback(
    callback: Option<RenderSurfaceLayoutBatchCallback>,
) {
    LAYOUT_BATCH_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

#[allow(unsafe_code)] // FFI callback is installed only by the public C++ bridge.
pub fn notify_render_surface_layout_batch(
    surface_id: i32,
    base_generation: u64,
    snapshots: &[RenderSurfaceLayoutSnapshot],
) {
    if snapshots.is_empty() {
        return;
    }
    LAYOUT_BATCH_CALLBACK.with(|slot| {
        if let Some(callback) = *slot.borrow() {
            unsafe {
                (callback.callback)(
                    surface_id,
                    base_generation,
                    snapshots.as_ptr(),
                    snapshots.len(),
                    callback.user_data,
                )
            };
        }
    });
}

/// Called by the shared render-surface drawing path once a frame has either
/// reached the renderer or has been rejected by clipping. Final OS compositor
/// presentation remains backend/driver controlled.
#[allow(unsafe_code)] // FFI callback is installed only by the public C++ bridge.
pub fn notify_render_surface_processed(surface_id: i32, generation: u64) {
    PROCESSED_CALLBACK.with(|slot| {
        if let Some(callback) = *slot.borrow() {
            unsafe { (callback.callback)(surface_id, generation, callback.user_data) };
        }
    });
}

/// Replaces the immutable frame associated with `surface_id`.
///
/// This is deliberately UI-thread local. Host applications must publish from
/// their event-loop callback, which also gives renderers a race-free snapshot.
pub fn publish_render_surface_frame(surface_id: i32, frame: RenderSurfaceFrame) {
    SURFACES.with(|surfaces| {
        surfaces.borrow_mut().insert(surface_id, Rc::new(frame));
    });
}

/// Replaces only the layers selected by `changed` while retaining the exact
/// immutable allocations for all omitted layers.
pub fn publish_render_surface_frame_delta(
    surface_id: i32,
    generation: u64,
    base_generation: u64,
    underlay_generation: u64,
    overlay_generation: u64,
    changed: RenderSurfaceLayerMask,
    base: Option<Rc<Vec<RenderSurfaceCommand>>>,
    underlay: Option<Rc<Vec<RenderSurfaceCommand>>>,
    overlay: Option<Rc<Vec<RenderSurfaceCommand>>>,
) {
    SURFACES.with(|surfaces| {
        let mut surfaces = surfaces.borrow_mut();
        let previous = surfaces.get(&surface_id);
        let empty = || Rc::new(Vec::new());
        let frame = RenderSurfaceFrame {
            generation,
            base_generation: if changed.contains(RenderSurfaceLayerMask::BASE) {
                base_generation
            } else {
                previous.map_or(0, |frame| frame.base_generation)
            },
            underlay_generation: if changed.contains(RenderSurfaceLayerMask::UNDERLAY) {
                underlay_generation
            } else {
                previous.map_or(0, |frame| frame.underlay_generation)
            },
            overlay_generation: if changed.contains(RenderSurfaceLayerMask::OVERLAY) {
                overlay_generation
            } else {
                previous.map_or(0, |frame| frame.overlay_generation)
            },
            commands: if changed.contains(RenderSurfaceLayerMask::BASE) {
                base.unwrap_or_else(empty)
            } else {
                previous.map_or_else(empty, |frame| frame.commands.clone())
            },
            underlay_commands: if changed.contains(RenderSurfaceLayerMask::UNDERLAY) {
                underlay.unwrap_or_else(empty)
            } else {
                previous.map_or_else(empty, |frame| frame.underlay_commands.clone())
            },
            overlay_commands: if changed.contains(RenderSurfaceLayerMask::OVERLAY) {
                overlay.unwrap_or_else(empty)
            } else {
                previous.map_or_else(empty, |frame| frame.overlay_commands.clone())
            },
        };
        surfaces.insert(surface_id, Rc::new(frame));
    });
}

/// Removes the frame for an inactive surface.
pub fn clear_render_surface_frame(surface_id: i32) {
    SURFACES.with(|surfaces| {
        surfaces.borrow_mut().remove(&surface_id);
    });
}

/// Returns the current immutable frame for `surface_id`.
pub fn render_surface_frame(surface_id: i32) -> Option<Rc<RenderSurfaceFrame>> {
    SURFACES.with(|surfaces| surfaces.borrow().get(&surface_id).cloned())
}

/// Lightweight rectangle adapter used by renderer implementations.
pub struct RenderSurfaceRectangle(pub Color);

impl RenderRectangle for RenderSurfaceRectangle {
    fn background(self: core::pin::Pin<&Self>) -> Brush {
        Brush::SolidColor(self.0)
    }
}

/// Lightweight text adapter used by renderer implementations.
pub struct RenderSurfaceTextRun {
    pub text: SharedString,
    pub color: Color,
    pub spans: Vec<RenderSurfaceTextSpan>,
    pub font: FontRequest,
    pub horizontal_alignment: TextHorizontalAlignment,
    pub vertical_alignment: TextVerticalAlignment,
}

impl HasFont for RenderSurfaceTextRun {
    fn font_request(self: core::pin::Pin<&Self>, _self_rc: &crate::items::ItemRc) -> FontRequest {
        self.font.clone()
    }
}

impl RenderString for RenderSurfaceTextRun {
    fn text(self: core::pin::Pin<&Self>) -> PlainOrStyledText {
        if self.spans.is_empty() {
            PlainOrStyledText::Plain(self.text.clone())
        } else {
            PlainOrStyledText::Styled(crate::styled_text::from_colored_spans(
                self.text.clone(),
                self.spans.iter().map(|span| {
                    (
                        Range { start: span.start_byte, end: span.end_byte },
                        span.color.as_argb_encoded(),
                    )
                }),
            ))
        }
    }
}

impl RenderText for RenderSurfaceTextRun {
    fn target_size(self: core::pin::Pin<&Self>) -> crate::lengths::LogicalSize {
        Default::default()
    }
    fn color(self: core::pin::Pin<&Self>) -> Brush {
        Brush::SolidColor(self.color)
    }
    fn link_color(self: core::pin::Pin<&Self>) -> Color {
        Default::default()
    }
    fn alignment(self: core::pin::Pin<&Self>) -> (TextHorizontalAlignment, TextVerticalAlignment) {
        (self.horizontal_alignment, self.vertical_alignment)
    }
    fn wrap(self: core::pin::Pin<&Self>) -> TextWrap {
        TextWrap::NoWrap
    }
    fn overflow(self: core::pin::Pin<&Self>) -> TextOverflow {
        TextOverflow::Clip
    }
    fn stroke(self: core::pin::Pin<&Self>) -> (Brush, LogicalLength, TextStrokeStyle) {
        Default::default()
    }
    fn is_markdown(self: core::pin::Pin<&Self>) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_replaces_and_clears_immutable_frames() {
        publish_render_surface_frame(
            17,
            RenderSurfaceFrame {
                generation: 1,
                base_generation: 1,
                underlay_generation: 1,
                overlay_generation: 1,
                commands: Rc::new(alloc::vec![RenderSurfaceCommand::FillRect {
                    x: 1.,
                    y: 2.,
                    width: 3.,
                    height: 4.,
                    color: Color::from_rgb_u8(1, 2, 3),
                }]),
                underlay_commands: Rc::new(Default::default()),
                overlay_commands: Rc::new(Default::default()),
            },
        );
        let first = render_surface_frame(17).unwrap();
        assert_eq!(first.generation, 1);
        publish_render_surface_frame(
            17,
            RenderSurfaceFrame {
                generation: 2,
                base_generation: 2,
                underlay_generation: 2,
                overlay_generation: 2,
                commands: Rc::new(Default::default()),
                underlay_commands: Rc::new(Default::default()),
                overlay_commands: Rc::new(Default::default()),
            },
        );
        assert_eq!(first.generation, 1);
        assert_eq!(render_surface_frame(17).unwrap().generation, 2);
        clear_render_surface_frame(17);
        assert!(render_surface_frame(17).is_none());
    }

    #[test]
    fn registry_delta_preserves_omitted_layers_and_clears_included_empty_layer() {
        let base = Rc::new(alloc::vec![RenderSurfaceCommand::FillRect {
            x: 0.,
            y: 0.,
            width: 1.,
            height: 1.,
            color: Color::default(),
        }]);
        publish_render_surface_frame(
            18,
            RenderSurfaceFrame {
                generation: 1,
                base_generation: 10,
                underlay_generation: 20,
                overlay_generation: 30,
                commands: base.clone(),
                underlay_commands: Rc::new(Default::default()),
                overlay_commands: Rc::new(Default::default()),
            },
        );
        publish_render_surface_frame_delta(
            18,
            2,
            11,
            21,
            31,
            RenderSurfaceLayerMask::UNDERLAY | RenderSurfaceLayerMask::OVERLAY,
            None,
            Some(Rc::new(Default::default())),
            Some(Rc::new(Default::default())),
        );
        let frame = render_surface_frame(18).unwrap();
        assert_eq!(frame.generation, 2);
        assert_eq!(frame.base_generation, 10);
        assert!(Rc::ptr_eq(&frame.commands, &base));
        assert!(frame.underlay_commands.is_empty());
        assert!(frame.overlay_commands.is_empty());
    }

    #[test]
    fn text_run_preserves_requested_alignment() {
        let run = RenderSurfaceTextRun {
            text: SharedString::from("text"),
            color: Color::default(),
            spans: Default::default(),
            font: Default::default(),
            horizontal_alignment: TextHorizontalAlignment::Right,
            vertical_alignment: TextVerticalAlignment::Center,
        };
        assert_eq!(
            core::pin::Pin::new(&run).alignment(),
            (TextHorizontalAlignment::Right, TextVerticalAlignment::Center)
        );
    }

    #[test]
    fn text_run_preserves_colored_utf8_spans() {
        let run = RenderSurfaceTextRun {
            text: SharedString::from("a·b"),
            color: Color::from_rgb_u8(1, 2, 3),
            spans: alloc::vec![RenderSurfaceTextSpan {
                start_byte: 1,
                end_byte: 3,
                color: Color::from_rgb_u8(4, 5, 6),
            }],
            font: Default::default(),
            horizontal_alignment: TextHorizontalAlignment::Left,
            vertical_alignment: TextVerticalAlignment::Top,
        };
        match core::pin::Pin::new(&run).text() {
            PlainOrStyledText::Styled(_) => {}
            PlainOrStyledText::Plain(_) => panic!("coloured span lost"),
        }
    }
}
