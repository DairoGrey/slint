// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! A small, renderer-backed display-list surface for host integrations.
//!
//! Native surfaces deliberately are not item trees. A producer publishes an
//! immutable bounded frame and Slint renders it inside one clipped item.

use crate::graphics::{Brush, Color, FontRequest};
use crate::item_rendering::{
    HasFont, PlainOrStyledText, RenderRectangle, RenderString, RenderText,
};
use crate::items::{TextHorizontalAlignment, TextOverflow, TextStrokeStyle, TextVerticalAlignment, TextWrap};
use crate::lengths::LogicalLength;
use crate::SharedString;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::ops::Range;
use core::cell::RefCell;
use core::ffi::c_void;
use crate::thread_local;

/// One immutable display list consumed by a [`crate::items::NativeSurfaceItem`].
#[derive(Clone, Default)]
pub struct NativeSurfaceFrame {
    /// Monotonically increasing producer generation. Renderers do not attach
    /// semantics to it, but it is useful for diagnostics and tests.
    pub generation: u64,
    /// Generation of immutable content commands.
    pub base_generation: u64,
    pub underlay_generation: u64,
    /// Generation of transient overlay commands.
    pub overlay_generation: u64,
    /// Commands are positioned in the local coordinate system of the item.
    pub commands: Rc<Vec<NativeSurfaceCommand>>,
    pub underlay_commands: Rc<Vec<NativeSurfaceCommand>>,
    /// Commands drawn after `commands`, for carets, selection and other
    /// transient overlays.
    pub overlay_commands: Rc<Vec<NativeSurfaceCommand>>,
}

/// A set of independently replaceable native-surface display-list layers.
///
/// A delta deliberately distinguishes an omitted layer from an explicitly
/// empty one: omitted layers retain their existing immutable list, while an
/// included empty layer clears that list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeSurfaceLayerMask(u8);

impl NativeSurfaceLayerMask {
    pub const BASE: Self = Self(1);
    pub const UNDERLAY: Self = Self(2);
    pub const OVERLAY: Self = Self(4);
    pub const ALL: Self = Self(Self::BASE.0 | Self::UNDERLAY.0 | Self::OVERLAY.0);

    pub const fn from_bits(bits: u8) -> Self { Self(bits & Self::ALL.0) }
    pub const fn contains(self, layer: Self) -> bool { self.0 & layer.0 != 0 }
}

impl core::ops::BitOr for NativeSurfaceLayerMask {
    type Output = Self;

    fn bitor(self, right: Self) -> Self::Output { Self::from_bits(self.0 | right.0) }
}

/// A primitive command accepted by native-surface renderers.
#[derive(Clone)]
pub enum NativeSurfaceCommand {
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
        spans: Vec<NativeSurfaceTextSpan>,
        font: FontRequest,
        horizontal_alignment: TextHorizontalAlignment,
        vertical_alignment: TextVerticalAlignment,
    },
    /// A horizontal or vertical solid line. Arbitrary angled paths are outside
    /// this intentionally small display-list contract.
    Line { x: f32, y: f32, width: f32, height: f32, color: Color },
}

/// One shaped text cluster in a native-surface text command. Coordinates are
/// local logical coordinates relative to the command's origin.
#[derive(Clone, Copy, Default)]
pub struct NativeSurfaceLayoutCluster {
    pub start_byte: u32,
    pub end_byte: u32,
    pub x: f32,
    pub width: f32,
}

/// Immutable post-shaping geometry for one text command. This is deliberately
/// renderer-neutral: hosts receive logical cluster positions, never renderer
/// objects or glyph cache handles.
#[derive(Clone, Default)]
pub struct NativeSurfaceLayoutSnapshot {
    pub layout_key: u64,
    pub baseline: f32,
    pub advance: f32,
    pub clusters: Vec<NativeSurfaceLayoutCluster>,
}

/// A foreground-colour override within a UTF-8 text command.
#[derive(Clone)]
pub struct NativeSurfaceTextSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub color: Color,
}

thread_local! {
    static SURFACES: RefCell<BTreeMap<i32, Rc<NativeSurfaceFrame>>> = Default::default();
    // The callback is intentionally UI-thread-local, just like the surface
    // registry. It is a diagnostic lifecycle hook for hosts; renderers do not
    // retain host state or depend on a callback being installed.
    static RENDERED_CALLBACK: RefCell<Option<NativeSurfaceRenderedCallback>> = Default::default();
    static LAYOUT_CALLBACK: RefCell<Option<NativeSurfaceLayoutCallback>> = Default::default();
}

#[derive(Clone, Copy)]
pub struct NativeSurfaceRenderedCallback {
    pub callback: unsafe extern "C" fn(i32, u64, *mut c_void),
    pub user_data: *mut c_void,
}

#[derive(Clone, Copy)]
pub struct NativeSurfaceLayoutCallback {
    pub callback: unsafe extern "C" fn(i32, u64, *const NativeSurfaceLayoutSnapshot, *mut c_void),
    pub user_data: *mut c_void,
}

/// Installs (or clears) the callback emitted after a native surface has been
/// rendered by the active backend. The callback runs on Slint's UI thread.
pub fn set_native_surface_rendered_callback(callback: Option<NativeSurfaceRenderedCallback>) {
    RENDERED_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

/// Installs the UI-thread callback that receives geometry from the same Parley
/// layout used to render each native-surface text command.
pub fn set_native_surface_layout_callback(callback: Option<NativeSurfaceLayoutCallback>) {
    LAYOUT_CALLBACK.with(|slot| *slot.borrow_mut() = callback);
}

#[allow(unsafe_code)] // FFI callback is installed only by the public C++ bridge.
pub fn notify_native_surface_layout(
    surface_id: i32,
    base_generation: u64,
    snapshot: &NativeSurfaceLayoutSnapshot,
) {
    if snapshot.layout_key == 0 {
        return;
    }
    LAYOUT_CALLBACK.with(|slot| {
        if let Some(callback) = *slot.borrow() {
            unsafe { (callback.callback)(surface_id, base_generation, snapshot as *const _, callback.user_data) };
        }
    });
}

/// Called by the shared native-surface drawing path after all three layers
/// have reached the renderer. This marks backend draw completion; final OS
/// compositor presentation remains backend/driver controlled.
#[allow(unsafe_code)] // FFI callback is installed only by the public C++ bridge.
pub fn notify_native_surface_rendered(surface_id: i32, generation: u64) {
    RENDERED_CALLBACK.with(|slot| {
        if let Some(callback) = *slot.borrow() {
            unsafe { (callback.callback)(surface_id, generation, callback.user_data) };
        }
    });
}

/// Replaces the immutable frame associated with `surface_id`.
///
/// This is deliberately UI-thread local. Host applications must publish from
/// their event-loop callback, which also gives renderers a race-free snapshot.
pub fn publish_native_surface_frame(surface_id: i32, frame: NativeSurfaceFrame) {
    SURFACES.with(|surfaces| {
        surfaces.borrow_mut().insert(surface_id, Rc::new(frame));
    });
}

/// Replaces only the layers selected by `changed` while retaining the exact
/// immutable allocations for all omitted layers.
pub fn publish_native_surface_frame_delta(
    surface_id: i32,
    generation: u64,
    base_generation: u64,
    underlay_generation: u64,
    overlay_generation: u64,
    changed: NativeSurfaceLayerMask,
    base: Option<Rc<Vec<NativeSurfaceCommand>>>,
    underlay: Option<Rc<Vec<NativeSurfaceCommand>>>,
    overlay: Option<Rc<Vec<NativeSurfaceCommand>>>,
) {
    SURFACES.with(|surfaces| {
        let mut surfaces = surfaces.borrow_mut();
        let previous = surfaces.get(&surface_id);
        let empty = || Rc::new(Vec::new());
        let frame = NativeSurfaceFrame {
            generation,
            base_generation: if changed.contains(NativeSurfaceLayerMask::BASE) {
                base_generation
            } else { previous.map_or(0, |frame| frame.base_generation) },
            underlay_generation: if changed.contains(NativeSurfaceLayerMask::UNDERLAY) {
                underlay_generation
            } else { previous.map_or(0, |frame| frame.underlay_generation) },
            overlay_generation: if changed.contains(NativeSurfaceLayerMask::OVERLAY) {
                overlay_generation
            } else { previous.map_or(0, |frame| frame.overlay_generation) },
            commands: if changed.contains(NativeSurfaceLayerMask::BASE) {
                base.unwrap_or_else(empty)
            } else { previous.map_or_else(empty, |frame| frame.commands.clone()) },
            underlay_commands: if changed.contains(NativeSurfaceLayerMask::UNDERLAY) {
                underlay.unwrap_or_else(empty)
            } else { previous.map_or_else(empty, |frame| frame.underlay_commands.clone()) },
            overlay_commands: if changed.contains(NativeSurfaceLayerMask::OVERLAY) {
                overlay.unwrap_or_else(empty)
            } else { previous.map_or_else(empty, |frame| frame.overlay_commands.clone()) },
        };
        surfaces.insert(surface_id, Rc::new(frame));
    });
}

/// Removes the frame for an inactive surface.
pub fn clear_native_surface_frame(surface_id: i32) {
    SURFACES.with(|surfaces| {
        surfaces.borrow_mut().remove(&surface_id);
    });
}

/// Returns the current immutable frame for `surface_id`.
pub fn native_surface_frame(surface_id: i32) -> Option<Rc<NativeSurfaceFrame>> {
    SURFACES.with(|surfaces| surfaces.borrow().get(&surface_id).cloned())
}

/// Lightweight rectangle adapter used by renderer implementations.
pub struct NativeSurfaceRectangle(pub Color);

impl RenderRectangle for NativeSurfaceRectangle {
    fn background(self: core::pin::Pin<&Self>) -> Brush { Brush::SolidColor(self.0) }
}

/// Lightweight text adapter used by renderer implementations.
pub struct NativeSurfaceTextRun {
    pub text: SharedString,
    pub color: Color,
    pub spans: Vec<NativeSurfaceTextSpan>,
    pub font: FontRequest,
    pub horizontal_alignment: TextHorizontalAlignment,
    pub vertical_alignment: TextVerticalAlignment,
}

impl HasFont for NativeSurfaceTextRun {
    fn font_request(self: core::pin::Pin<&Self>, _self_rc: &crate::items::ItemRc) -> FontRequest {
        self.font.clone()
    }
}

impl RenderString for NativeSurfaceTextRun {
    fn text(self: core::pin::Pin<&Self>) -> PlainOrStyledText {
        if self.spans.is_empty() {
            PlainOrStyledText::Plain(self.text.clone())
        } else {
            PlainOrStyledText::Styled(crate::styled_text::from_colored_spans(
                self.text.clone(),
                self.spans.iter().map(|span| (Range {
                    start: span.start_byte,
                    end: span.end_byte,
                }, span.color.as_argb_encoded())),
            ))
        }
    }
}

impl RenderText for NativeSurfaceTextRun {
    fn target_size(self: core::pin::Pin<&Self>) -> crate::lengths::LogicalSize { Default::default() }
    fn color(self: core::pin::Pin<&Self>) -> Brush { Brush::SolidColor(self.color) }
    fn link_color(self: core::pin::Pin<&Self>) -> Color { Default::default() }
    fn alignment(self: core::pin::Pin<&Self>) -> (TextHorizontalAlignment, TextVerticalAlignment) {
        (self.horizontal_alignment, self.vertical_alignment)
    }
    fn wrap(self: core::pin::Pin<&Self>) -> TextWrap { TextWrap::NoWrap }
    fn overflow(self: core::pin::Pin<&Self>) -> TextOverflow { TextOverflow::Clip }
    fn stroke(self: core::pin::Pin<&Self>) -> (Brush, LogicalLength, TextStrokeStyle) { Default::default() }
    fn is_markdown(self: core::pin::Pin<&Self>) -> bool { false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_replaces_and_clears_immutable_frames() {
        publish_native_surface_frame(17, NativeSurfaceFrame {
            generation: 1,
            base_generation: 1,
            underlay_generation: 1,
            overlay_generation: 1,
            commands: Rc::new(alloc::vec![NativeSurfaceCommand::FillRect {
                x: 1., y: 2., width: 3., height: 4., color: Color::from_rgb_u8(1, 2, 3),
            }]),
            underlay_commands: Rc::new(Default::default()),
            overlay_commands: Rc::new(Default::default()),
        });
        let first = native_surface_frame(17).unwrap();
        assert_eq!(first.generation, 1);
        publish_native_surface_frame(17, NativeSurfaceFrame {
            generation: 2, base_generation: 2, underlay_generation: 2, overlay_generation: 2,
            commands: Rc::new(Default::default()), underlay_commands: Rc::new(Default::default()), overlay_commands: Rc::new(Default::default()),
        });
        assert_eq!(first.generation, 1);
        assert_eq!(native_surface_frame(17).unwrap().generation, 2);
        clear_native_surface_frame(17);
        assert!(native_surface_frame(17).is_none());
    }

    #[test]
    fn registry_delta_preserves_omitted_layers_and_clears_included_empty_layer() {
        let base = Rc::new(alloc::vec![NativeSurfaceCommand::FillRect {
            x: 0., y: 0., width: 1., height: 1., color: Color::default(),
        }]);
        publish_native_surface_frame(18, NativeSurfaceFrame {
            generation: 1, base_generation: 10, underlay_generation: 20, overlay_generation: 30,
            commands: base.clone(), underlay_commands: Rc::new(Default::default()), overlay_commands: Rc::new(Default::default()),
        });
        publish_native_surface_frame_delta(18, 2, 11, 21, 31,
            NativeSurfaceLayerMask::UNDERLAY | NativeSurfaceLayerMask::OVERLAY,
            None, Some(Rc::new(Default::default())), Some(Rc::new(Default::default())));
        let frame = native_surface_frame(18).unwrap();
        assert_eq!(frame.generation, 2);
        assert_eq!(frame.base_generation, 10);
        assert!(Rc::ptr_eq(&frame.commands, &base));
        assert!(frame.underlay_commands.is_empty());
        assert!(frame.overlay_commands.is_empty());
    }

    #[test]
    fn text_run_preserves_requested_alignment() {
        let run = NativeSurfaceTextRun {
            text: SharedString::from("text"),
            color: Color::default(),
            spans: Default::default(),
            font: Default::default(),
            horizontal_alignment: TextHorizontalAlignment::Right,
            vertical_alignment: TextVerticalAlignment::Center,
        };
        assert_eq!(core::pin::Pin::new(&run).alignment(),
            (TextHorizontalAlignment::Right, TextVerticalAlignment::Center));
    }

    #[test]
    fn text_run_preserves_colored_utf8_spans() {
        let run = NativeSurfaceTextRun {
            text: SharedString::from("a·b"),
            color: Color::from_rgb_u8(1, 2, 3),
            spans: alloc::vec![NativeSurfaceTextSpan {
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
