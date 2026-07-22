// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Software-3.0

//! Platform text-input connection for externally owned document models.

use super::{
    EventResult, FocusEvent, FocusEventResult, FocusReason, InputEventFilterResult,
    InputEventResult, Item, ItemConsts, ItemRc, ItemRendererRef, KeyEventArg, KeyEventResult,
    LayoutInfo, LogicalLength, LogicalRect, LogicalSize, MouseCursorInner, RenderingResult,
    StringArg, VoidArg,
};
use crate::input::{InternalKeyEvent, KeyEventType, MouseEvent, StandardShortcut};
use crate::item_rendering::CachedRenderingData;
use crate::platform::Clipboard;
use crate::properties::ChangeTracker;
#[cfg(feature = "rtti")]
use crate::rtti::*;
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
pub struct OverlayTextInputItem {
    pub enabled: Property<bool>,
    pub has_focus: Property<bool>,
    pub focus_on_click: Property<bool>,
    pub input_method_hints: Property<super::InputMethodHints>,
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
    pub text_input: Callback<StringArg>,
    pub preedit_updated: Callback<StringArg>,
    pub composition_committed: Callback<StringArg>,
    pub composition_cancelled: Callback<VoidArg>,
    pub copy_requested: Callback<VoidArg>,
    pub cut_requested: Callback<VoidArg>,
    pub clipboard_written: Callback<VoidArg>,
    pub paste_received: Callback<StringArg>,
    pub cached_rendering_data: CachedRenderingData,
    last_input_generation: Cell<i32>,
    last_surrounding_text: Property<SharedString>,
    last_preedit_text: Property<SharedString>,
    last_cursor_offset: Cell<i32>,
    last_anchor_offset: Cell<i32>,
    last_caret_x: Cell<f32>,
    last_caret_y: Cell<f32>,
    last_caret_width: Cell<f32>,
    last_caret_height: Cell<f32>,
    submitted_input_generation: Cell<i32>,
    input_method_enabled: Cell<bool>,
    last_clipboard_generation: Cell<i32>,
    input_change_tracker: ChangeTracker,
}

#[derive(Default, PartialEq)]
struct EffectiveInputState {
    enabled: bool,
    has_focus: bool,
    surrounding_text: SharedString,
    cursor_offset: i32,
    anchor_offset: i32,
    preedit_text: SharedString,
    caret_x: f32,
    caret_y: f32,
    caret_width: f32,
    caret_height: f32,
    input_generation: i32,
    clipboard_write_text: SharedString,
    clipboard_write_generation: i32,
}

impl OverlayTextInputItem {
    fn is_printable_key_text(event: &InternalKeyEvent) -> bool {
        let modifiers = event.key_event.modifiers;
        if modifiers.control || modifiers.alt || modifiers.meta {
            return false;
        }
        !event.key_event.text.is_empty()
            && event.key_event.text.chars().all(|ch| {
                !ch.is_control()
                    && ch != '\u{fffd}'
                    && !matches!(ch,
                        '\u{e000}'..='\u{f8ff}'
                        | '\u{f0000}'..='\u{ffffd}'
                        | '\u{100000}'..='\u{10fffd}')
            })
    }

    fn accepts_platform_input(self: Pin<&Self>) -> bool {
        self.enabled()
            && self.has_focus()
            && self.input_method_enabled.get()
            && self.submitted_input_generation.get() == self.input_generation()
    }

    fn dispatch_preedit(self: Pin<&Self>, event: &InternalKeyEvent) -> bool {
        if !self.accepts_platform_input() {
            return false;
        }
        self.event_input_generation.set(self.submitted_input_generation.get());
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
        true
    }

    fn dispatch_commit(self: Pin<&Self>, event: &InternalKeyEvent) -> bool {
        if !self.accepts_platform_input() {
            return false;
        }
        self.event_input_generation.set(self.submitted_input_generation.get());
        let replacement = event.replacement_range.clone().unwrap_or(0..0);
        self.preedit_text.set(Default::default());
        self.replacement_start.set(replacement.start);
        self.replacement_end.set(replacement.end);
        self.composition_committed.call(&(event.key_event.text.clone(),));
        true
    }

    fn ime_properties(self: Pin<&Self>, self_rc: &ItemRc) -> InputMethodProperties {
        let text = self.surrounding_text();
        let cursor = self.cursor_offset().clamp(0, text.len() as i32) as usize;
        let anchor = self.anchor_offset().clamp(0, text.len() as i32) as usize;
        let geometry = self_rc.geometry();
        let item_origin = self_rc.map_to_native_window(geometry.origin).to_vector();
        let cursor_origin = crate::api::LogicalPosition::from_euclid(
            crate::api::LogicalPosition::new(self.caret_x().get(), self.caret_y().get())
                .to_euclid()
                + item_origin,
        );
        let cursor_size = crate::api::LogicalSize::new(
            self.caret_width().get().max(1.),
            self.caret_height().get().max(1.),
        );
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
                cursor_origin.x + cursor_size.width,
                cursor_origin.y + cursor_size.height,
            ),
            input_type: super::InputType::Text,
            input_method_hints: self.input_method_hints(),
            clip_rect,
        }
    }

    fn record_effective_input_state(self: Pin<&Self>) -> bool {
        let generation = self.input_generation();
        let surrounding = self.surrounding_text();
        let preedit = self.preedit_text();
        let cursor = self.cursor_offset();
        let anchor = self.anchor_offset();
        let caret_x = self.caret_x().get();
        let caret_y = self.caret_y().get();
        let caret_width = self.caret_width().get();
        let caret_height = self.caret_height().get();
        let mut changed = self.last_input_generation.replace(generation) != generation;
        changed |= self.last_surrounding_text() != surrounding;
        changed |= self.last_preedit_text() != preedit;
        changed |= self.last_cursor_offset.replace(cursor) != cursor;
        changed |= self.last_anchor_offset.replace(anchor) != anchor;
        // A candidate rectangle matters while an IME composition is active.
        // Polling it unconditionally made every smooth-scroll render send a
        // synchronous platform input-method update even though no candidate
        // window existed. Keep the cached geometry current, but only make a
        // geometry-only change observable while preedit is active. A later
        // preedit/content change still sends the then-current rectangle.
        let geometry_changed = self.last_caret_x.replace(caret_x) != caret_x
            || self.last_caret_y.replace(caret_y) != caret_y
            || self.last_caret_width.replace(caret_width) != caret_width
            || self.last_caret_height.replace(caret_height) != caret_height;
        changed |= !preedit.is_empty() && geometry_changed;
        if changed {
            self.last_surrounding_text.set(surrounding);
            self.last_preedit_text.set(preedit);
        }
        changed
    }

    fn mark_input_state_submitted(self: Pin<&Self>) {
        self.as_ref().record_effective_input_state();
        self.submitted_input_generation.set(self.input_generation());
    }

    fn enable_input_method(self: Pin<&Self>, window_adapter: &WindowAdapterRc, self_rc: &ItemRc) {
        WindowInner::from_pub(window_adapter.window()).set_text_input_focused(true);
        if let Some(window) = window_adapter.internal(crate::InternalToken) {
            window.input_method_request(InputMethodRequest::Enable(self.ime_properties(self_rc)));
        }
        self.input_method_enabled.set(true);
        self.mark_input_state_submitted();
    }

    fn disable_input_method(self: Pin<&Self>, window_adapter: &WindowAdapterRc) {
        if self.input_method_enabled.replace(false) {
            // WindowInner is the sole owner of the platform Disable request.
            WindowInner::from_pub(window_adapter.window()).set_text_input_focused(false);
        }
    }

    fn cancel_composition(self: Pin<&Self>) {
        if !self.preedit_text().is_empty() {
            self.event_input_generation.set(self.submitted_input_generation.get());
            self.preedit_text.set(Default::default());
            self.composition_cancelled.call(&());
        }
    }

    fn update_input_method(self: Pin<&Self>, window_adapter: &WindowAdapterRc, self_rc: &ItemRc) {
        if !self.has_focus() || !self.enabled() {
            if !self.enabled() {
                self.cancel_composition();
            }
            self.disable_input_method(window_adapter);
            return;
        }
        if !self.input_method_enabled.get() {
            self.enable_input_method(window_adapter, self_rc);
            return;
        }
        if !self.record_effective_input_state() {
            return;
        }
        if let Some(window) = window_adapter.internal(crate::InternalToken) {
            window.input_method_request(InputMethodRequest::Update(self.ime_properties(self_rc)));
        }
        self.submitted_input_generation.set(self.input_generation());
    }

    fn process_platform_state(
        self: Pin<&Self>,
        window_adapter: &WindowAdapterRc,
        self_rc: &ItemRc,
    ) {
        self.update_input_method(window_adapter, self_rc);
        let generation = self.clipboard_write_generation();
        if generation >= 0 && generation > self.last_clipboard_generation.get() {
            WindowInner::from_pub(window_adapter.window())
                .context()
                .platform()
                .set_clipboard_text(&self.clipboard_write_text(), Clipboard::DefaultClipboard);
            self.last_clipboard_generation.set(generation);
            self.clipboard_written_generation.set(generation);
            self.clipboard_written.call(&());
        }
    }
}

impl Item for OverlayTextInputItem {
    fn init(self: Pin<&Self>, self_rc: &ItemRc) {
        self.last_input_generation.set(i32::MIN);
        self.last_cursor_offset.set(i32::MIN);
        self.last_anchor_offset.set(i32::MIN);
        self.last_caret_x.set(f32::NAN);
        self.last_caret_y.set(f32::NAN);
        self.last_caret_width.set(f32::NAN);
        self.last_caret_height.set(f32::NAN);
        self.submitted_input_generation.set(i32::MIN);
        self.input_method_enabled.set(false);
        self.last_clipboard_generation.set(-1);
        self.clipboard_written_generation.set(-1);
        self.input_change_tracker.init(
            self_rc.downgrade(),
            |weak| {
                let Some(item_rc) = weak.upgrade() else { return EffectiveInputState::default() };
                let Some(item) = item_rc.downcast::<OverlayTextInputItem>() else {
                    return EffectiveInputState::default();
                };
                let item = item.as_pin_ref();
                let preedit_text = item.preedit_text();
                let composition_active = !preedit_text.is_empty();
                EffectiveInputState {
                    enabled: item.enabled(),
                    has_focus: item.has_focus(),
                    surrounding_text: item.surrounding_text(),
                    cursor_offset: item.cursor_offset(),
                    anchor_offset: item.anchor_offset(),
                    preedit_text,
                    caret_x: composition_active.then(|| item.caret_x().get()).unwrap_or_default(),
                    caret_y: composition_active.then(|| item.caret_y().get()).unwrap_or_default(),
                    caret_width: composition_active
                        .then(|| item.caret_width().get())
                        .unwrap_or_default(),
                    caret_height: composition_active
                        .then(|| item.caret_height().get())
                        .unwrap_or_default(),
                    input_generation: item.input_generation(),
                    clipboard_write_text: item.clipboard_write_text(),
                    clipboard_write_generation: item.clipboard_write_generation(),
                }
            },
            |weak, _| {
                let Some(item_rc) = weak.upgrade() else { return };
                let Some(item) = item_rc.downcast::<OverlayTextInputItem>() else { return };
                let Some(window) = item_rc.window_adapter() else { return };
                item.as_pin_ref().process_platform_state(&window, &item_rc);
            },
        );
    }

    fn deinit(self: Pin<&Self>, window_adapter: &WindowAdapterRc) {
        self.input_change_tracker.clear();
        self.disable_input_method(window_adapter);
    }

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
        _event: &MouseEvent,
        _window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
        _cursor: &mut MouseCursorInner,
    ) -> InputEventFilterResult {
        InputEventFilterResult::ForwardEvent
    }

    fn input_event(
        self: Pin<&Self>,
        event: &MouseEvent,
        window_adapter: &WindowAdapterRc,
        self_rc: &ItemRc,
        _cursor: &mut MouseCursorInner,
    ) -> InputEventResult {
        if self.enabled()
            && self.focus_on_click()
            && matches!(event, MouseEvent::Pressed { .. })
            && !self.has_focus()
        {
            WindowInner::from_pub(window_adapter.window()).set_focus_item(
                self_rc,
                true,
                FocusReason::PointerClick,
            );
            InputEventResult::EventAccepted
        } else {
            InputEventResult::EventIgnored
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
        event: &InternalKeyEvent,
        window_adapter: &WindowAdapterRc,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        if !self.enabled() || !self.has_focus() {
            return KeyEventResult::EventIgnored;
        }
        match event.event_type {
            KeyEventType::KeyPressed => {
                let platform =
                    || WindowInner::from_pub(window_adapter.window()).context().platform();
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
                    EventResult::Reject if Self::is_printable_key_text(event) => {
                        self.text_input.call(&(event.key_event.text.clone(),));
                        KeyEventResult::EventAccepted
                    }
                    EventResult::Reject => KeyEventResult::EventIgnored,
                }
            }
            KeyEventType::KeyReleased => {
                match self.key_released.call(&(event.key_event.clone(),)) {
                    EventResult::Accept => KeyEventResult::EventAccepted,
                    EventResult::Reject => KeyEventResult::EventIgnored,
                }
            }
            KeyEventType::UpdateComposition => {
                if self.dispatch_preedit(event) {
                    KeyEventResult::EventAccepted
                } else {
                    KeyEventResult::EventIgnored
                }
            }
            KeyEventType::CommitComposition => {
                if self.dispatch_commit(event) {
                    KeyEventResult::EventAccepted
                } else {
                    KeyEventResult::EventIgnored
                }
            }
        }
    }

    fn focus_event(
        self: Pin<&Self>,
        event: &FocusEvent,
        window_adapter: &WindowAdapterRc,
        self_rc: &ItemRc,
    ) -> FocusEventResult {
        if !self.enabled() {
            return FocusEventResult::FocusIgnored;
        }
        match event {
            FocusEvent::FocusIn(_) => {
                self.has_focus.set(true);
                self.enable_input_method(window_adapter, self_rc);
            }
            FocusEvent::FocusOut(reason) => {
                self.has_focus.set(false);
                self.disable_input_method(window_adapter);
                if !matches!(reason, FocusReason::WindowActivation | FocusReason::PopupActivation) {
                    self.cancel_composition();
                }
            }
        }
        FocusEventResult::FocusAccepted
    }

    fn render(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        self_rc: &ItemRc,
        _size: LogicalSize,
    ) -> RenderingResult {
        if let Some(window) = self_rc.window_adapter() {
            self.process_platform_state(&window, self_rc);
        }
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
        false
    }
}

impl ItemConsts for OverlayTextInputItem {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        OverlayTextInputItem,
        CachedRenderingData,
    > = OverlayTextInputItem::FIELD_OFFSETS.cached_rendering_data().as_unpinned_projection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use core::cell::RefCell;

    fn activate(item: Pin<&OverlayTextInputItem>, generation: i32) {
        item.enabled.set(true);
        item.has_focus.set(true);
        item.input_generation.set(generation);
        item.submitted_input_generation.set(generation);
        item.input_method_enabled.set(true);
    }

    #[test]
    fn preedit_and_commit_keep_external_ranges() {
        let item = Box::pin(OverlayTextInputItem::default());
        activate(item.as_ref(), 7);
        let preedit = Rc::new(RefCell::new(SharedString::default()));
        let observed = preedit.clone();
        item.preedit_updated.set_handler(move |(text,)| *observed.borrow_mut() = text.clone());
        assert!(item.as_ref().dispatch_preedit(&InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            preedit_text: "é".into(),
            replacement_range: Some(-2..1),
            preedit_selection: Some(0..2),
            ..Default::default()
        }));
        assert_eq!(&**preedit.borrow(), "é");
        assert_eq!(item.as_ref().replacement_start(), -2);
        assert_eq!(item.as_ref().replacement_end(), 1);
        assert_eq!(item.as_ref().preedit_selection_start(), 0);
        assert_eq!(item.as_ref().preedit_selection_end(), 2);
        assert_eq!(item.as_ref().event_input_generation(), 7);

        let committed = Rc::new(RefCell::new(SharedString::default()));
        let observed = committed.clone();
        item.composition_committed
            .set_handler(move |(text,)| *observed.borrow_mut() = text.clone());
        let mut event = InternalKeyEvent {
            event_type: KeyEventType::CommitComposition,
            replacement_range: Some(-2..1),
            ..Default::default()
        };
        event.key_event.text = "界".into();
        assert!(item.as_ref().dispatch_commit(&event));
        assert_eq!(&**committed.borrow(), "界");
        assert!(item.as_ref().preedit_text().is_empty());
    }

    #[test]
    fn empty_preedit_cancels_without_commit() {
        let item = Box::pin(OverlayTextInputItem::default());
        activate(item.as_ref(), 3);
        let cancelled = Rc::new(Cell::new(false));
        let observed = cancelled.clone();
        item.composition_cancelled.set_handler(move |()| observed.set(true));
        assert!(item.as_ref().dispatch_preedit(&InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            ..Default::default()
        }));
        assert!(cancelled.get());
    }

    #[test]
    fn stale_or_unfocused_composition_is_rejected() {
        let item = Box::pin(OverlayTextInputItem::default());
        activate(item.as_ref(), 4);
        let observed = Rc::new(Cell::new(0));
        let callback_observed = observed.clone();
        item.preedit_updated
            .set_handler(move |_| callback_observed.set(callback_observed.get() + 1));
        let event = InternalKeyEvent {
            event_type: KeyEventType::UpdateComposition,
            preedit_text: "compose".into(),
            ..Default::default()
        };

        item.input_generation.set(5);
        assert!(!item.as_ref().dispatch_preedit(&event));
        assert_eq!(observed.get(), 0);
        assert!(item.as_ref().preedit_text().is_empty());

        item.submitted_input_generation.set(5);
        assert!(item.as_ref().dispatch_preedit(&event));
        assert_eq!(observed.get(), 1);
        assert_eq!(item.as_ref().event_input_generation(), 5);

        item.has_focus.set(false);
        assert!(!item.as_ref().dispatch_preedit(&event));
        assert_eq!(observed.get(), 1);
    }

    #[test]
    fn printable_key_text_excludes_modifiers_and_platform_key_codes() {
        let event = |text: &str, modifiers| {
            let mut event = InternalKeyEvent::default();
            event.key_event.text = text.into();
            event.key_event.modifiers = modifiers;
            event
        };

        assert!(OverlayTextInputItem::is_printable_key_text(&event("界", Default::default())));
        assert!(OverlayTextInputItem::is_printable_key_text(&event(
            "A",
            crate::input::KeyboardModifiers { shift: true, ..Default::default() }
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            "\u{f700}",
            Default::default()
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            "\u{fffd}",
            Default::default()
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            "x",
            crate::input::KeyboardModifiers { meta: true, ..Default::default() }
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            " ",
            crate::input::KeyboardModifiers { meta: true, ..Default::default() }
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            " ",
            crate::input::KeyboardModifiers { control: true, ..Default::default() }
        )));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event(
            "x",
            crate::input::KeyboardModifiers { alt: true, shift: true, ..Default::default() }
        )));
        // The core modifier tracker removes Ctrl/Alt from a key event that was
        // actually produced through AltGr, leaving the resulting text printable.
        assert!(OverlayTextInputItem::is_printable_key_text(&event("€", Default::default())));
        assert!(!OverlayTextInputItem::is_printable_key_text(&event("\n", Default::default())));
    }

    #[test]
    fn candidate_geometry_updates_only_while_composition_is_active() {
        let item = Box::pin(OverlayTextInputItem::default());
        assert!(!item.as_ref().record_effective_input_state());
        item.caret_y.set(LogicalLength::new(24.));
        assert!(!item.as_ref().record_effective_input_state());
        item.preedit_text.set("compose".into());
        assert!(item.as_ref().record_effective_input_state());
        item.caret_y.set(LogicalLength::new(48.));
        assert!(item.as_ref().record_effective_input_state());
        assert!(!item.as_ref().record_effective_input_state());
        item.surrounding_text.set("context".into());
        assert!(item.as_ref().record_effective_input_state());
    }
}
