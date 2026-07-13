// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*! This crate just exposes the function used by the C++ integration */

#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::rc::Rc;
use alloc::string::ToString;
use core::cell::RefCell;
use core::ffi::c_void;
use i_slint_core::SharedString;
use i_slint_core::graphics::{Color, FontRequest};
use i_slint_core::lengths::LogicalLength;
use i_slint_core::native_surface::{
    NativeSurfaceCommand, NativeSurfaceFrame, NativeSurfaceLayerMask, clear_native_surface_frame,
    publish_native_surface_frame, publish_native_surface_frame_delta,
    NativeSurfaceLayoutCallback, NativeSurfaceRenderedCallback, set_native_surface_layout_callback,
    set_native_surface_rendered_callback,
};
use i_slint_core::items::OperatingSystemType;
use i_slint_core::items::{TextHorizontalAlignment, TextVerticalAlignment};
use i_slint_core::slice::Slice;
use i_slint_core::styled_text::StyledText;
use i_slint_core::window::{WindowAdapter, ffi::WindowAdapterRcOpaque};

type NativeSurfaceLayoutCxxCallback = unsafe extern "C" fn(
    i32, u64, *const NativeSurfaceLayoutSnapshotData, *mut c_void,
);

i_slint_core::thread_local! {
    static NATIVE_SURFACE_LAYOUT_CXX_CALLBACK: RefCell<Option<(NativeSurfaceLayoutCxxCallback, *mut c_void)>> = Default::default();
}

pub mod platform;

#[repr(C)]
pub struct NativeSurfaceCommandData {
    kind: u8,
    layout_key: u64,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color_argb: u32,
    text: *const u8,
    text_len: usize,
    text_spans: *const NativeSurfaceTextSpanData,
    text_span_count: usize,
    font_family: *const u8,
    font_family_len: usize,
    font_size: f32,
    font_weight: i32,
    horizontal_alignment: u8,
    vertical_alignment: u8,
}

#[repr(C)]
pub struct NativeSurfaceTextSpanData {
    start_byte: u32,
    end_byte: u32,
    color_argb: u32,
}

#[repr(C)]
pub struct NativeSurfaceLayoutClusterData {
    start_byte: u32,
    end_byte: u32,
    x: f32,
    width: f32,
}

#[repr(C)]
pub struct NativeSurfaceLayoutSnapshotData {
    layout_key: u64,
    baseline: f32,
    advance: f32,
    clusters: *const NativeSurfaceLayoutClusterData,
    cluster_count: usize,
}

unsafe extern "C" fn native_surface_layout_callback_bridge(
    surface_id: i32,
    base_generation: u64,
    snapshot: *const i_slint_core::native_surface::NativeSurfaceLayoutSnapshot,
    _user_data: *mut c_void,
) {
    let Some(snapshot) = (unsafe { snapshot.as_ref() }) else { return; };
    NATIVE_SURFACE_LAYOUT_CXX_CALLBACK.with(|slot| {
        let Some((callback, user_data)) = *slot.borrow() else { return; };
        let clusters = snapshot.clusters.iter().map(|cluster| NativeSurfaceLayoutClusterData {
            start_byte: cluster.start_byte,
            end_byte: cluster.end_byte,
            x: cluster.x,
            width: cluster.width,
        }).collect::<alloc::vec::Vec<_>>();
        let payload = NativeSurfaceLayoutSnapshotData {
            layout_key: snapshot.layout_key,
            baseline: snapshot.baseline,
            advance: snapshot.advance,
            clusters: clusters.as_ptr(),
            cluster_count: clusters.len(),
        };
        unsafe { callback(surface_id, base_generation, &payload, user_data) };
    });
}

fn ffi_string(data: *const u8, len: usize) -> SharedString {
    if data.is_null() || len == 0 {
        return SharedString::default();
    }
    let bytes = unsafe { core::slice::from_raw_parts(data, len) };
    core::str::from_utf8(bytes).map(SharedString::from).unwrap_or_default()
}

fn ffi_text_spans(data: *const NativeSurfaceTextSpanData, len: usize, text_len: usize)
    -> alloc::vec::Vec<i_slint_core::native_surface::NativeSurfaceTextSpan>
{
    if data.is_null() || len == 0 {
        return Default::default();
    }
    unsafe { core::slice::from_raw_parts(data, len) }
        .iter()
        .filter_map(|span| {
            let start = span.start_byte as usize;
            let end = span.end_byte as usize;
            (start < end && end <= text_len).then_some(i_slint_core::native_surface::NativeSurfaceTextSpan {
                start_byte: start,
                end_byte: end,
                color: Color::from_argb_encoded(span.color_argb),
            })
        })
        .collect()
}

unsafe fn native_surface_commands(
    commands: *const NativeSurfaceCommandData,
    command_count: usize,
) -> alloc::vec::Vec<NativeSurfaceCommand> {
    let commands = if commands.is_null() || command_count == 0 { &[] }
        else { unsafe { core::slice::from_raw_parts(commands, command_count) } };
    let mut result = alloc::vec::Vec::with_capacity(commands.len());
    for command in commands {
        let color = Color::from_argb_encoded(command.color_argb);
        result.push(match command.kind {
            0 => NativeSurfaceCommand::FillRect { x: command.x, y: command.y, width: command.width, height: command.height, color },
            1 => NativeSurfaceCommand::Text {
                layout_key: command.layout_key,
                x: command.x, y: command.y, width: command.width, height: command.height,
                text: ffi_string(command.text, command.text_len), color,
                spans: ffi_text_spans(command.text_spans, command.text_span_count, command.text_len),
                font: FontRequest { family: Some(ffi_string(command.font_family, command.font_family_len)),
                    weight: Some(command.font_weight), pixel_size: Some(LogicalLength::new(command.font_size)), ..Default::default() },
                horizontal_alignment: match command.horizontal_alignment { 1 => TextHorizontalAlignment::Center, 2 => TextHorizontalAlignment::Right, _ => TextHorizontalAlignment::Left },
                vertical_alignment: match command.vertical_alignment { 1 => TextVerticalAlignment::Center, 2 => TextVerticalAlignment::Bottom, _ => TextVerticalAlignment::Top },
            },
            _ => NativeSurfaceCommand::Line { x: command.x, y: command.y, width: command.width, height: command.height, color },
        });
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_native_surface_publish(
    surface_id: i32,
    generation: u64,
    commands: *const NativeSurfaceCommandData,
    command_count: usize,
) {
    let frame = NativeSurfaceFrame {
        generation,
        base_generation: generation,
        underlay_generation: generation,
        overlay_generation: generation,
        commands: Rc::new(unsafe { native_surface_commands(commands, command_count) }),
        underlay_commands: Rc::new(Default::default()),
        overlay_commands: Rc::new(Default::default()),
    };
    publish_native_surface_frame(surface_id, frame);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_native_surface_publish_layers(
    surface_id: i32, generation: u64, base_generation: u64, underlay_generation: u64, overlay_generation: u64,
    base: *const NativeSurfaceCommandData, base_count: usize,
    underlay: *const NativeSurfaceCommandData, underlay_count: usize,
    overlay: *const NativeSurfaceCommandData, overlay_count: usize,
) {
    publish_native_surface_frame(surface_id, NativeSurfaceFrame {
        generation, base_generation, underlay_generation, overlay_generation,
        commands: Rc::new(unsafe { native_surface_commands(base, base_count) }),
        underlay_commands: Rc::new(unsafe { native_surface_commands(underlay, underlay_count) }),
        overlay_commands: Rc::new(unsafe { native_surface_commands(overlay, overlay_count) }),
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_native_surface_publish_layers_delta(
    surface_id: i32, generation: u64, base_generation: u64, underlay_generation: u64, overlay_generation: u64,
    changed_layers: u8,
    base: *const NativeSurfaceCommandData, base_count: usize,
    underlay: *const NativeSurfaceCommandData, underlay_count: usize,
    overlay: *const NativeSurfaceCommandData, overlay_count: usize,
) {
    let changed = NativeSurfaceLayerMask::from_bits(changed_layers);
    publish_native_surface_frame_delta(surface_id, generation, base_generation, underlay_generation, overlay_generation,
        changed,
        changed.contains(NativeSurfaceLayerMask::BASE)
            .then(|| Rc::new(unsafe { native_surface_commands(base, base_count) })),
        changed.contains(NativeSurfaceLayerMask::UNDERLAY)
            .then(|| Rc::new(unsafe { native_surface_commands(underlay, underlay_count) })),
        changed.contains(NativeSurfaceLayerMask::OVERLAY)
            .then(|| Rc::new(unsafe { native_surface_commands(overlay, overlay_count) })),
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_native_surface_clear(surface_id: i32) {
    clear_native_surface_frame(surface_id);
}

/// Registers a UI-thread callback invoked after a native surface frame has
/// been drawn by Slint's renderer. This is deliberately a draw-completion
/// hook rather than a platform-specific swap/vsync promise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_native_surface_set_rendered_callback(
    callback: Option<unsafe extern "C" fn(i32, u64, *mut c_void)>,
    user_data: *mut c_void,
) {
    set_native_surface_rendered_callback(callback.map(|callback| NativeSurfaceRenderedCallback {
        callback,
        user_data,
    }));
}

/// Registers a callback for immutable post-shaping text geometry. Pointers are
/// valid for the duration of the callback only and the callback runs on the
/// Slint UI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_native_surface_set_layout_callback(
    callback: Option<unsafe extern "C" fn(i32, u64, *const NativeSurfaceLayoutSnapshotData, *mut c_void)>,
    user_data: *mut c_void,
) {
    NATIVE_SURFACE_LAYOUT_CXX_CALLBACK.with(|slot| *slot.borrow_mut() = callback.map(|callback| (callback, user_data)));
    set_native_surface_layout_callback(callback.map(|_| NativeSurfaceLayoutCallback {
        callback: native_surface_layout_callback_bridge,
        user_data: core::ptr::null_mut(),
    }));
}

#[cfg(feature = "i-slint-backend-selector")]
use i_slint_backend_selector::with_platform;

#[cfg(not(feature = "i-slint-backend-selector"))]
pub fn with_platform<R>(
    f: impl FnOnce(
        &dyn i_slint_core::platform::Platform,
    ) -> Result<R, i_slint_core::platform::PlatformError>,
) -> Result<R, i_slint_core::platform::PlatformError> {
    i_slint_core::with_platform(|| Err(i_slint_core::platform::PlatformError::NoPlatform), f)
}

// We need to make sure something from the crate is exported,
// otherwise its symbols are not going to be in the final binary
#[cfg(feature = "testing")]
pub use i_slint_backend_testing;
#[cfg(feature = "slint-interpreter")]
pub use slint_interpreter;

#[cfg(feature = "live-preview")]
pub use i_slint_live_preview;

#[cfg(target_os = "android")]
mod android {
    unsafe extern "C" {
        fn slint_main();
    }

    #[unsafe(no_mangle)]
    fn android_main(app: i_slint_backend_android_activity::AndroidApp) {
        i_slint_core::platform::set_platform(alloc::boxed::Box::new(
            i_slint_backend_android_activity::AndroidPlatform::new(app),
        ))
        .unwrap();
        #[cfg(any(feature = "mcp", feature = "system-testing"))]
        i_slint_backend_selector::init_testing_backends();
        unsafe { slint_main() };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_context_accent_color(
    root: &i_slint_core::item_tree::ItemTreeRc,
    out: &mut i_slint_core::graphics::Color,
) {
    *out = i_slint_core::window::accent_color(root);
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_context_color_scheme(
    root: &i_slint_core::item_tree::ItemTreeRc,
) -> i_slint_core::items::ColorScheme {
    i_slint_core::window::context_for_root(root)
        .map_or(i_slint_core::items::ColorScheme::Unknown, |ctx| ctx.color_scheme(Some(root)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_windowrc_init(out: *mut WindowAdapterRcOpaque) {
    assert_eq!(
        core::mem::size_of::<Rc<dyn WindowAdapter>>(),
        core::mem::size_of::<WindowAdapterRcOpaque>()
    );
    let win = with_platform(|b| b.create_window_adapter()).unwrap();
    unsafe {
        core::ptr::write(out as *mut Rc<dyn WindowAdapter>, win);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_ensure_backend() {
    with_platform(|_b| {
        // Nothing to do, just make sure a backend was created
        Ok(())
    })
    .unwrap()
}

#[unsafe(no_mangle)]
/// Enters the main event loop.
pub extern "C" fn slint_run_event_loop(quit_on_last_window_closed: bool) {
    with_platform(|b| {
        if !quit_on_last_window_closed {
            #[allow(deprecated)]
            b.set_event_loop_quit_on_last_window_closed(false);
        }
        b.run_event_loop()
    })
    .unwrap();
}

/// Will execute the given functor in the main thread
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_post_event(
    event: extern "C" fn(user_data: *mut c_void),
    user_data: *mut c_void,
    drop_user_data: Option<extern "C" fn(*mut c_void)>,
) {
    struct UserData {
        user_data: *mut c_void,
        drop_user_data: Option<extern "C" fn(*mut c_void)>,
    }
    impl Drop for UserData {
        fn drop(&mut self) {
            if let Some(x) = self.drop_user_data {
                x(self.user_data)
            }
        }
    }
    unsafe impl Send for UserData {}
    let ud = UserData { user_data, drop_user_data };

    i_slint_core::api::invoke_from_event_loop(move || {
        let ud = &ud;
        event(ud.user_data);
    })
    .unwrap();
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_quit_event_loop() {
    i_slint_core::api::quit_event_loop().unwrap();
}

#[cfg(feature = "std")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_register_font_from_path(
    win: *const WindowAdapterRcOpaque,
    path: &SharedString,
    error_str: &mut SharedString,
) {
    let window_adapter = unsafe { &*(win as *const Rc<dyn WindowAdapter>) };
    *error_str = match window_adapter
        .renderer()
        .register_font_from_path(std::path::Path::new(path.as_str()))
    {
        Ok(()) => Default::default(),
        Err(err) => i_slint_core::string::ToSharedString::to_shared_string(&err),
    };
}

#[cfg(feature = "std")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_register_font_from_data(
    win: *const WindowAdapterRcOpaque,
    data: i_slint_core::slice::Slice<'static, u8>,
    error_str: &mut SharedString,
) {
    let window_adapter = unsafe { &*(win as *const Rc<dyn WindowAdapter>) };
    *error_str = match window_adapter.renderer().register_font_from_memory(data.as_slice()) {
        Ok(()) => Default::default(),
        Err(err) => i_slint_core::string::ToSharedString::to_shared_string(&err),
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_register_bitmap_font(
    win: *const WindowAdapterRcOpaque,
    font_data: &'static i_slint_core::graphics::BitmapFont,
) {
    let window_adapter = unsafe { &*(win as *const Rc<dyn WindowAdapter>) };
    window_adapter.renderer().register_bitmap_font(font_data);
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_string_to_float(string: &SharedString, value: &mut f32) -> bool {
    if let Some(v) = i_slint_core::string::string_to_float(string.as_str()) {
        *value = v;
        true
    } else {
        false
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_string_character_count(string: &SharedString) -> usize {
    unicode_segmentation::UnicodeSegmentation::graphemes(string.as_str(), true).count()
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_string_to_usize(string: &SharedString, value: &mut usize) -> bool {
    match string.as_str().parse::<usize>() {
        Ok(v) => {
            *value = v;
            true
        }
        Err(_) => false,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_debug(string: &SharedString) {
    i_slint_core::debug_log!("{string}");
}

#[cfg(not(feature = "std"))]
mod allocator {
    use core::alloc::Layout;
    use core::ffi::c_void;

    struct CAlloc;
    unsafe impl core::alloc::GlobalAlloc for CAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            unsafe extern "C" {
                pub fn malloc(size: usize) -> *mut c_void;
            }
            unsafe {
                let align = layout.align();
                if align <= core::mem::size_of::<usize>() {
                    malloc(layout.size()) as *mut u8
                } else {
                    // Ideally we'd use aligned_alloc, but that function caused heap corruption with esp-idf
                    let ptr = malloc(layout.size() + align) as *mut u8;
                    let shift = align - (ptr as usize % align);
                    let ptr = ptr.add(shift);
                    core::ptr::write(ptr.sub(1), shift as u8);
                    ptr
                }
            }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            let align = layout.align();
            unsafe extern "C" {
                pub fn free(p: *mut c_void);
            }
            unsafe {
                if align <= core::mem::size_of::<usize>() {
                    free(ptr as *mut c_void);
                } else {
                    let shift = core::ptr::read(ptr.sub(1)) as usize;
                    free(ptr.sub(shift) as *mut c_void);
                }
            }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CAlloc = CAlloc;
}

#[cfg(all(not(feature = "std"), not(feature = "esp-backtrace")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
#[cfg(feature = "esp-backtrace")]
use esp_backtrace as _;

#[unsafe(no_mangle)]
pub extern "C" fn slint_set_xdg_app_id(_app_id: &SharedString) {
    #[cfg(feature = "i-slint-backend-selector")]
    i_slint_backend_selector::with_global_context(|ctx| ctx.set_xdg_app_id(_app_id.clone()))
        .unwrap();
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_detect_operating_system() -> OperatingSystemType {
    i_slint_core::detect_operating_system()
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_parse_markdown(
    format_string: &SharedString,
    args: Slice<StyledText>,
    out: &mut StyledText,
) {
    *out = i_slint_core::styled_text::parse_markdown(format_string, &args);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_open_url(
    url: &SharedString,
    win: *const WindowAdapterRcOpaque,
) -> bool {
    let window_adapter = unsafe { &*(win as *const Rc<dyn WindowAdapter>) };
    i_slint_core::open_url(url, window_adapter.window()).is_ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_macos_bring_all_windows_to_front() {
    i_slint_core::macos_bring_all_windows_to_front()
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_string_to_styled_text(text: &SharedString, out: &mut StyledText) {
    *out = i_slint_core::styled_text::string_to_styled_text(text.to_string());
}

#[unsafe(no_mangle)]
pub extern "C" fn slint_color_to_styled_text(color: &i_slint_core::Color, out: &mut StyledText) {
    *out = i_slint_core::styled_text::color_to_styled_text(*color);
}

// Translator API is currently considered experimental due to discussions
// about the returned string type (SharedString vs. Cow<str> etc.). Also it
// is not available with no_std due to the tr crate.
// See discussion in https://github.com/slint-ui/slint/pull/10979.
#[cfg(all(feature = "experimental", feature = "std"))]
mod translator {
    use crate::SharedString;
    use crate::Slice;
    use alloc::boxed::Box;
    use core::ffi::c_void;
    use i_slint_core::translations::Translator;
    use std::borrow::Cow;

    type DropCallback = extern "C" fn(obj: *const c_void);

    type TranslateCallback = extern "C" fn(
        obj: *const c_void,
        string: Slice<u8>,
        context: Slice<u8>,
        out: &mut SharedString,
    );

    type NTranslateCallback = extern "C" fn(
        obj: *const c_void,
        n: u64,
        singular: Slice<u8>,
        plural: Slice<u8>,
        context: Slice<u8>,
        out: &mut SharedString,
    );

    struct CppTranslator {
        pub obj: *const c_void,
        pub drop: DropCallback,
        pub translate: TranslateCallback,
        pub ntranslate: NTranslateCallback,
    }

    unsafe impl Send for CppTranslator {}
    unsafe impl Sync for CppTranslator {}

    impl Drop for CppTranslator {
        fn drop(&mut self) {
            (self.drop)(self.obj);
        }
    }

    impl Translator for CppTranslator {
        fn translate<'a>(&'a self, string: &'a str, context: Option<&'a str>) -> Cow<'a, str> {
            let mut out = SharedString::new();
            (self.translate)(
                self.obj,
                string.as_bytes().into(),
                context.unwrap_or_default().as_bytes().into(),
                &mut out,
            );
            Cow::Owned(out.into())
        }

        fn ntranslate<'a>(
            &'a self,
            n: u64,
            singular: &'a str,
            plural: &'a str,
            context: Option<&'a str>,
        ) -> Cow<'a, str> {
            let mut out = SharedString::new();
            (self.ntranslate)(
                self.obj,
                n,
                singular.as_bytes().into(),
                plural.as_bytes().into(),
                context.unwrap_or_default().as_bytes().into(),
                &mut out,
            );
            Cow::Owned(out.into())
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn slint_translate_set_translator(
        obj: *const c_void,
        drop: DropCallback,
        translate: TranslateCallback,
        ntranslate: NTranslateCallback,
    ) -> bool {
        #[cfg(feature = "i-slint-backend-selector")]
        i_slint_backend_selector::with_global_context(|ctx| {
            if !obj.is_null() {
                ctx.set_external_translator(Some(Box::new(CppTranslator {
                    obj,
                    drop,
                    translate,
                    ntranslate,
                })))
            } else {
                ctx.set_external_translator(None)
            }
        })
        .is_ok()
    }
}
