// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Software-3.0

//! Platform text-input connection for externally owned document models.

use super::{
    EventResult, FocusEvent, FocusEventResult, FocusReason, InputEventFilterResult,
    InputEventResult, Item, ItemConsts, ItemRc, ItemRendererRef, KeyEventArg, KeyEventResult,
    LayoutInfo, LogicalLength, LogicalRect, LogicalSize, MouseCursor, RenderingResult,
    StringArg, VoidArg,
};
use crate::input::{InternalKeyEvent, KeyEventType, MouseEvent, StandardShortcut};
use crate::item_rendering::CachedRenderingData;
use crate::platform::Clipboard;
use crate::window::{InputMethodProperties, InputMethodRequest, WindowAdapterRc, WindowInner};
use crate::{Callback, Coord, Property, SharedString};
use const_field_offset::FieldOffsets;
use core::cell::Cell;
use core::pin::Pin;
use i_slint_core_macros::*;

#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
/// Focus and platform IME bridge for text stored outside Slint.
pub struct ExternalTextInputItem {
    pub enabled: Property<bool>,
    pub has_focus: Property<bool>,
    pub focus_on_click: Property<bool>,
    pub surrounding_text: Property<SharedString>,
    pub cursor_offset: Property<i32>,
    pub anchor_offset: Property<i32>,
    pub preedit_text: Property<SharedString>,
    pub caret_x: Property<LogicalLength>,
    pub caret_y: Property<LogicalLength>,
    pub caret_width: Property<LogicalLength>,
    pub caret_height: Property<LogicalLength>,
    pub input_generation: Property<i32>,
    pub clipboard_write_text: Property<SharedString>,
    pub clipboard_write_generation: Property<i32>,
    pub event_input_generation: Property<i32>,
    pub replacement_start: Property<i32>,
    pub replacement_end: Property<i32>,
    pub preedit_selection_start: Property<i32>,
    pub preedit_selection_end: Property<i32>,
    pub clipboard_written_generation: Property<i32>,
    pub key_pressed: Callback<KeyEventArg, EventResult>,
    pub key_released: Callback<KeyEventArg, EventResult>,
    pub preedit_updated: Callback<StringArg>,
    pub composition_committed: Callback<StringArg>,
    pub composition_cancelled: Callback<VoidArg>,
    pub copy_requested: Callback<VoidArg>,
    pub cut_requested: Callback<VoidArg>,
    pub clipboard_written: Callback<VoidArg>,
    pub paste_received: Callback<StringArg>,
    pub cached_rendering_data: CachedRenderingData,
    last_input_generation: Cell<i32>,
    last_clipboard_generation: Cell<i32>,
}

impl ExternalTextInputItem {
    fn dispatch_preedit(self: Pin<&Self>, event: &InternalKeyEvent) {
        self.event_input_generation.set(self.input_generation());
        let replacement = event.replacement_range.clone().unwrap_or(0..0);
        let selection = event.preedit_selection.clone().unwrap_or(-1..-1);
        self.preedit_text.set(event.preedit_text.clone());
        if event.preedit_text.is_empty() {
            self.composition_cancelled.call(&());
        } else {
            self.replacement_start.set(replacement.start);
            self.replacement_end.set(replacement.end);
            self.preedit_selection_start.set(selection.start);
            self.preedit_selection_end.set(selection.end);
            self.preedit_updated.call(&(event.preedit_text.clone(),));
        }
    }

    fn dispatch_commit(self: Pin<&Self>, event: &InternalKeyEvent) {
        self.event_input_generation.set(self.input_generation());
        let replacement = event.replacement_range.clone().unwrap_or(0..0);
        self.preedit_text.set(Default::default());
        self.replacement_start.set(replacement.start);
        self.replacement_end.set(replacement.end);
        self.composition_committed.call(&(event.key_event.text.clone(),));
    }

    fn ime_properties(self: Pin<&Self>, self_rc: &ItemRc) -> InputMethodProperties {
        let text = self.surrounding_text();
        let cursor = self.cursor_offset().clamp(0, text.len() as i32) as usize;
        let anchor = self.anchor_offset().clamp(0, text.len() as i32) as usize;
        let geometry = self_rc.geometry();
        let item_origin = self_rc.map_to_native_window(geometry.origin).to_vector();
        let cursor_origin = crate::api::LogicalPosition::from_euclid(
            crate::api::LogicalPosition::new(self.caret_x().get(), self.caret_y().get()).to_euclid()
                + item_origin,
        );
        let cursor_size = crate::api::LogicalSize::new(
            self.caret_width().get().max(1.), self.caret_height().get().max(1.));
        let clip_rect = self_rc
            .parent_item(crate::item_tree::ParentItemTraversalMode::StopAtPopups)
            .map(|parent| {
                let geometry = parent.geometry();
                LogicalRect::new(parent.map_to_native_window(geometry.origin), geometry.size)
            });
        InputMethodProperties {
            text,
            cursor_position: cursor,
            anchor_position: (anchor != cursor).then_some(anchor),
            preedit_text: self.preedit_text(),
            preedit_offset: cursor,
            cursor_rect_origin: cursor_origin,
            cursor_rect_size: cursor_size,
            anchor_point: crate::api::LogicalPosition::new(
                cursor_origin.x + cursor_size.width, cursor_origin.y + cursor_size.height),
            input_type: super::InputType::Text,
            clip_rect,
        }
    }

    fn update_input_method(self: Pin<&Self>, window_adapter: &WindowAdapterRc, self_rc: &ItemRc) {
        if !self.has_focus() || !self.enabled() { return; }
        let generation = self.input_generation();
        if self.last_input_generation.replace(generation) == generation { return; }
        if let Some(window) = window_adapter.internal(crate::InternalToken) {
            window.input_method_request(InputMethodRequest::Update(self.ime_properties(self_rc)));
        }
    }
}

impl Item for ExternalTextInputItem {
    fn init(self: Pin<&Self>, _self_rc: &ItemRc) {
        self.last_input_generation.set(i32::MIN);
        self.last_clipboard_generation.set(i32::MIN);
    }

    fn deinit(self: Pin<&Self>, window_adapter: &WindowAdapterRc) {
        if self.has_focus() {
            if let Some(window) = window_adapter.internal(crate::InternalToken) {
                window.input_method_request(InputMethodRequest::Disable);
            }
        }
    }

    fn layout_info(self: Pin<&Self>, _orientation: super::Orientation, _cross_axis_constraint: Coord,
        _window_adapter: &WindowAdapterRc, _self_rc: &ItemRc) -> LayoutInfo
    {
        LayoutInfo { stretch: 1., ..LayoutInfo::default() }
    }

    fn input_event_filter_before_children(self: Pin<&Self>, _event: &MouseEvent,
        _window_adapter: &WindowAdapterRc, _self_rc: &ItemRc, _cursor: &mut MouseCursor)
        -> InputEventFilterResult
    {
        InputEventFilterResult::ForwardEvent
    }

    fn input_event(self: Pin<&Self>, event: &MouseEvent, window_adapter: &WindowAdapterRc,
        self_rc: &ItemRc, _cursor: &mut MouseCursor) -> InputEventResult
    {
        if self.enabled() && self.focus_on_click() && matches!(event, MouseEvent::Pressed { .. })
            && !self.has_focus()
        {
            WindowInner::from_pub(window_adapter.window()).set_focus_item(
                self_rc, true, FocusReason::PointerClick);
            InputEventResult::EventAccepted
        } else {
            InputEventResult::EventIgnored
        }
    }

    fn capture_key_event(self: Pin<&Self>, _event: &InternalKeyEvent,
        _window_adapter: &WindowAdapterRc, _self_rc: &ItemRc) -> KeyEventResult
    {
        KeyEventResult::EventIgnored
    }

    fn key_event(self: Pin<&Self>, event: &InternalKeyEvent, window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc) -> KeyEventResult
    {
        match event.event_type {
            KeyEventType::KeyPressed => {
                let platform = || WindowInner::from_pub(window_adapter.window()).context().platform();
                match event.shortcut() {
                    Some(StandardShortcut::Copy) => {
                        self.copy_requested.call(&());
                        return KeyEventResult::EventAccepted;
                    }
                    Some(StandardShortcut::Cut) => {
                        self.cut_requested.call(&());
                        return KeyEventResult::EventAccepted;
                    }
                    Some(StandardShortcut::Paste) => {
                        if let Some(text) = platform().clipboard_text(Clipboard::DefaultClipboard) {
                            self.paste_received.call(&(text.into(),));
                        }
                        return KeyEventResult::EventAccepted;
                    }
                    _ => {}
                }
                match self.key_pressed.call(&(event.key_event.clone(),)) {
                    EventResult::Accept => KeyEventResult::EventAccepted,
                    EventResult::Reject => KeyEventResult::EventIgnored,
                }
            }
            KeyEventType::KeyReleased => match self.key_released.call(&(event.key_event.clone(),)) {
                EventResult::Accept => KeyEventResult::EventAccepted,
                EventResult::Reject => KeyEventResult::EventIgnored,
            },
            KeyEventType::UpdateComposition => {
                self.dispatch_preedit(event);
                KeyEventResult::EventAccepted
            }
            KeyEventType::CommitComposition => {
                self.dispatch_commit(event);
                KeyEventResult::EventAccepted
            }
        }
    }

    fn focus_event(self: Pin<&Self>, event: &FocusEvent, window_adapter: &WindowAdapterRc,
        self_rc: &ItemRc) -> FocusEventResult
    {
        if !self.enabled() { return FocusEventResult::FocusIgnored; }
        match event {
            FocusEvent::FocusIn(_) => {
                self.has_focus.set(true);
                self.last_input_generation.set(self.input_generation());
                WindowInner::from_pub(window_adapter.window()).set_text_input_focused(true);
                if let Some(window) = window_adapter.internal(crate::InternalToken) {
                    window.input_method_request(InputMethodRequest::Enable(self.ime_properties(self_rc)));
                }
            }
            FocusEvent::FocusOut(reason) => {
                self.has_focus.set(false);
                WindowInner::from_pub(window_adapter.window()).set_text_input_focused(false);
                if !matches!(reason, FocusReason::WindowActivation | FocusReason::PopupActivation) {
                    if !self.preedit_text().is_empty() {
                        self.event_input_generation.set(self.input_generation());
                        self.preedit_text.set(Default::default());
                        self.composition_cancelled.call(&());
                    }
                    if let Some(window) = window_adapter.internal(crate::InternalToken) {
                        window.input_method_request(InputMethodRequest::Disable);
                    }
                }
            }
        }
        FocusEventResult::FocusAccepted
    }

    fn render(self: Pin<&Self>, _backend: &mut ItemRendererRef, self_rc: &ItemRc,
        _size: LogicalSize) -> RenderingResult
    {
        if let Some(window) = self_rc.window_adapter() {
            self.update_input_method(&window, self_rc);
            let generation = self.clipboard_write_generation();
            if generation >= 0 && self.last_clipboard_generation.replace(generation) != generation {
                WindowInner::from_pub(window.window()).context().platform().set_clipboard_text(
                    &self.clipboard_write_text(), Clipboard::DefaultClipboard);
                self.clipboard_written_generation.set(generation);
                self.clipboard_written.call(&());
            }
        }
        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(self: Pin<&Self>, _window_adapter: &WindowAdapterRc, _self_rc: &ItemRc,
        geometry: LogicalRect) -> LogicalRect { geometry }
    fn clips_children(self: Pin<&Self>) -> bool { false }
}

impl ItemConsts for ExternalTextInputItem {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        ExternalTextInputItem, CachedRenderingData,
    > = ExternalTextInputItem::FIELD_OFFSETS.cached_rendering_data().as_unpinned_projection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use core::cell::RefCell;

    #[test]
    fn preedit_and_commit_keep_external_ranges() {
        let item = Box::pin(ExternalTextInputItem::default());
        let preedit = Rc::new(RefCell::new(SharedString::default()));
        let observed = preedit.clone();
        item.preedit_updated.set_handler(move |(text,)| *observed.borrow_mut() = text.clone());
        item.as_ref().dispatch_preedit(&InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            preedit_text: "é".into(),
            replacement_range: Some(-2..1),
            preedit_selection: Some(0..2),
            ..Default::default()
        });
        assert_eq!(&**preedit.borrow(), "é");
        assert_eq!(item.as_ref().replacement_start(), -2);
        assert_eq!(item.as_ref().replacement_end(), 1);
        assert_eq!(item.as_ref().preedit_selection_start(), 0);
        assert_eq!(item.as_ref().preedit_selection_end(), 2);
        assert_eq!(item.as_ref().event_input_generation(), 0);

        let committed = Rc::new(RefCell::new(SharedString::default()));
        let observed = committed.clone();
        item.composition_committed.set_handler(move |(text,)| *observed.borrow_mut() = text.clone());
        let mut event = InternalKeyEvent { event_type: KeyEventType::CommitComposition,
            replacement_range: Some(-2..1), ..Default::default() };
        event.key_event.text = "界".into();
        item.as_ref().dispatch_commit(&event);
        assert_eq!(&**committed.borrow(), "界");
        assert!(item.as_ref().preedit_text().is_empty());
    }

    #[test]
    fn empty_preedit_cancels_without_commit() {
        let item = Box::pin(ExternalTextInputItem::default());
        let cancelled = Rc::new(Cell::new(false));
        let observed = cancelled.clone();
        item.composition_cancelled.set_handler(move |()| observed.set(true));
        item.as_ref().dispatch_preedit(&InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            ..Default::default()
        });
        assert!(cancelled.get());
    }
}
