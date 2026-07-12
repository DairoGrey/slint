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
use crate::thread_local;

/// One immutable display list consumed by a [`crate::items::NativeSurfaceItem`].
#[derive(Clone, Default)]
pub struct NativeSurfaceFrame {
    /// Monotonically increasing producer generation. Renderers do not attach
    /// semantics to it, but it is useful for diagnostics and tests.
    pub generation: u64,
    /// Commands are positioned in the local coordinate system of the item.
    pub commands: Vec<NativeSurfaceCommand>,
}

/// A primitive command accepted by native-surface renderers.
#[derive(Clone)]
pub enum NativeSurfaceCommand {
    /// A solid filled rectangle.
    FillRect { x: f32, y: f32, width: f32, height: f32, color: Color },
    /// A text run with an explicit font request and local origin.
    Text {
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

/// A foreground-colour override within a UTF-8 text command.
#[derive(Clone)]
pub struct NativeSurfaceTextSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub color: Color,
}

thread_local! {
    static SURFACES: RefCell<BTreeMap<i32, Rc<NativeSurfaceFrame>>> = Default::default();
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
            commands: alloc::vec![NativeSurfaceCommand::FillRect {
                x: 1., y: 2., width: 3., height: 4., color: Color::from_rgb_u8(1, 2, 3),
            }],
        });
        let first = native_surface_frame(17).unwrap();
        assert_eq!(first.generation, 1);
        publish_native_surface_frame(17, NativeSurfaceFrame { generation: 2, commands: Default::default() });
        assert_eq!(first.generation, 1);
        assert_eq!(native_surface_frame(17).unwrap().generation, 2);
        clear_native_surface_frame(17);
        assert!(native_surface_frame(17).is_none());
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
