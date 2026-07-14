// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! The builtin item backing the public render-surface display-list API.

use super::{
    EventResult, FocusEvent, FocusEventResult, InputEventFilterResult, InputEventResult, Item,
    ItemConsts, ItemRc, ItemRendererRef, KeyEventResult, LogicalLength, LogicalRect, LogicalSize,
    MouseCursor, PointerEvent, PointerEventArg, PointerEventButton, PointerEventKind,
    PointerScrollEvent, PointerScrollEventArg, RenderingResult,
};
use crate::input::{InternalKeyEvent, MouseEvent};
use crate::item_rendering::CachedRenderingData;
use crate::layout::LayoutInfo;
use crate::lengths::PointLengths;
#[cfg(feature = "rtti")]
use crate::rtti::*;
use crate::window::WindowAdapterRc;
use crate::{Callback, Coord, Property};
use const_field_offset::FieldOffsets;
use core::cell::Cell;
use core::pin::Pin;
use i_slint_core_macros::*;

#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
/// One clipped input and display-list surface.
pub struct RenderSurfaceItem {
    pub surface_id: Property<i32>,
    pub frame_generation: Property<i32>,
    pub content_offset_x: Property<LogicalLength>,
    pub content_offset_y: Property<LogicalLength>,
    pub enabled: Property<bool>,
    pub mouse_cursor: Property<MouseCursor>,
    pub mouse_x: Property<LogicalLength>,
    pub mouse_y: Property<LogicalLength>,
    pub pointer_event: Callback<PointerEventArg>,
    pub scroll_event: Callback<PointerScrollEventArg, EventResult>,
    pub cached_rendering_data: CachedRenderingData,
    grabbed: Cell<bool>,
}

impl RenderSurfaceItem {
    fn notify_pointer(self: Pin<&Self>, event: &MouseEvent, window_adapter: &WindowAdapterRc) {
        let Some(position) = event.position() else { return };
        Self::FIELD_OFFSETS.mouse_x().apply_pin(self).set(position.x_length());
        Self::FIELD_OFFSETS.mouse_y().apply_pin(self).set(position.y_length());
        let (button, kind, touch_finger_id) = match event {
            MouseEvent::Pressed { button, touch_finger_id, .. } => {
                (*button, PointerEventKind::Down, *touch_finger_id)
            }
            MouseEvent::Released { button, touch_finger_id, .. } => {
                (*button, PointerEventKind::Up, *touch_finger_id)
            }
            MouseEvent::Moved { touch_finger_id, .. } => {
                (PointerEventButton::Other, PointerEventKind::Move, *touch_finger_id)
            }
            _ => return,
        };
        Self::FIELD_OFFSETS.pointer_event().apply_pin(self).call(&(PointerEvent {
            button,
            kind,
            modifiers: window_adapter.window().0.context().0.modifiers.get().into(),
            touch_finger_id,
        },));
    }
}

impl Item for RenderSurfaceItem {
    fn init(self: Pin<&Self>, _self_rc: &ItemRc) {}

    fn deinit(self: Pin<&Self>, _window_adapter: &WindowAdapterRc) {}

    fn layout_info(
        self: Pin<&Self>,
        _orientation: super::Orientation,
        _cross_axis_constraint: Coord,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
    ) -> LayoutInfo {
        LayoutInfo { stretch: 1., ..LayoutInfo::default() }
    }

    fn input_event_filter_before_children(
        self: Pin<&Self>,
        event: &MouseEvent,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
        cursor: &mut MouseCursor,
    ) -> InputEventFilterResult {
        if !self.enabled() || matches!(event, MouseEvent::DragMove { .. } | MouseEvent::Drop { .. })
        {
            return InputEventFilterResult::ForwardAndIgnore;
        }
        if event.position().is_some() && !matches!(event, MouseEvent::Exit) {
            *cursor = self.mouse_cursor();
            InputEventFilterResult::ForwardAndInterceptGrab
        } else {
            InputEventFilterResult::ForwardAndIgnore
        }
    }

    fn input_event(
        self: Pin<&Self>,
        event: &MouseEvent,
        window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
        _cursor: &mut MouseCursor,
    ) -> InputEventResult {
        if !self.enabled() {
            return InputEventResult::EventIgnored;
        }
        match event {
            MouseEvent::Pressed { .. } => {
                self.grabbed.set(true);
                self.notify_pointer(event, window_adapter);
                InputEventResult::GrabMouse
            }
            MouseEvent::Released { .. } => {
                self.grabbed.set(false);
                self.notify_pointer(event, window_adapter);
                InputEventResult::EventAccepted
            }
            MouseEvent::Moved { .. } => {
                self.notify_pointer(event, window_adapter);
                if self.grabbed.get() {
                    InputEventResult::GrabMouse
                } else {
                    InputEventResult::EventAccepted
                }
            }
            MouseEvent::Wheel { delta_x, delta_y, .. } => {
                let modifiers = window_adapter.window().0.context().0.modifiers.get().into();
                match Self::FIELD_OFFSETS.scroll_event().apply_pin(self).call(&(
                    PointerScrollEvent { delta_x: *delta_x, delta_y: *delta_y, modifiers },
                )) {
                    EventResult::Accept => InputEventResult::EventAccepted,
                    EventResult::Reject => InputEventResult::EventIgnored,
                }
            }
            MouseEvent::Exit => {
                self.grabbed.set(false);
                Self::FIELD_OFFSETS.pointer_event().apply_pin(self).call(&(PointerEvent {
                    button: PointerEventButton::Other,
                    kind: PointerEventKind::Cancel,
                    modifiers: window_adapter.window().0.context().0.modifiers.get().into(),
                    touch_finger_id: 0,
                },));
                InputEventResult::EventAccepted
            }
            MouseEvent::PinchGesture { .. }
            | MouseEvent::RotationGesture { .. }
            | MouseEvent::DragMove { .. }
            | MouseEvent::Drop { .. } => InputEventResult::EventIgnored,
        }
    }

    fn capture_key_event(
        self: Pin<&Self>,
        _event: &InternalKeyEvent,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        _event: &InternalKeyEvent,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _event: &FocusEvent,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        FocusEventResult::FocusIgnored
    }

    fn render(
        self: Pin<&Self>,
        backend: &mut ItemRendererRef,
        self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        // Register the generated property with Slint's dirty tracker. The
        // frame registry itself deliberately is not a property store.
        let _ = self.frame_generation();
        (*backend).draw_render_surface(self, self_rc, size);
        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(
        self: Pin<&Self>,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
        geometry: LogicalRect,
    ) -> LogicalRect {
        geometry
    }

    fn clips_children(self: Pin<&Self>) -> bool {
        true
    }
}

impl ItemConsts for RenderSurfaceItem {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        RenderSurfaceItem,
        CachedRenderingData,
    > = RenderSurfaceItem::FIELD_OFFSETS.cached_rendering_data().as_unpinned_projection();
}
