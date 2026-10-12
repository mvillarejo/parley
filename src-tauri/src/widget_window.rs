//! The floating recorder window (`widget`). On macOS it is turned into a
//! non-activating panel, so clicking Mark, Pause or Stop never takes focus
//! from the call app. It accepts keyboard focus only while the user types
//! (the meeting title or a note), and hands it back afterwards.

use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

pub const LABEL: &str = "widget";
const WIDTH: f64 = 340.0;
/// The size in `tauri.conf.json`.
pub const COMPACT_HEIGHT: f64 = 132.0;
/// With the note field open.
pub const NOTE_HEIGHT: f64 = 172.0;

/// What the recorder is being typed into, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    None,
    Title,
    Note,
}

impl InputMode {
    pub fn height(self) -> f64 {
        match self {
            InputMode::Note => NOTE_HEIGHT,
            InputMode::None | InputMode::Title => COMPACT_HEIGHT,
        }
    }

    pub fn wants_keyboard(self) -> bool {
        self != InputMode::None
    }
}

/// Make the recorder a non-activating panel (macOS). Call once at startup.
pub fn init(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window(LABEL) {
        panel::convert(&window);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Size the recorder for `mode` and give it the keyboard only while typing.
pub fn set_input(app: &AppHandle, mode: InputMode) -> Result<(), String> {
    let window = app
        .get_webview_window(LABEL)
        .ok_or("The recorder window is missing")?;
    resize_keeping_top(&window, mode.height());
    #[cfg(target_os = "macos")]
    if panel::is_panel() {
        return panel::set_keyboard(&window, mode.wants_keyboard());
    }
    // A regular window takes the keyboard by being focused, like any other.
    if mode.wants_keyboard() {
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Grow or shrink downwards: the title row stays where the user put it.
fn resize_keeping_top(window: &WebviewWindow, height: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let Ok(size) = window.inner_size() else {
        return;
    };
    if (size.to_logical::<f64>(scale).height - height).abs() < 0.5 {
        return;
    }
    let position = window.outer_position().ok();
    let _ = window.set_size(LogicalSize::new(WIDTH, height));
    if let Some(position) = position {
        let _ = window.set_position(position);
    }
}

#[cfg(target_os = "macos")]
mod panel {
    use objc2_06::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2_06::{msg_send, sel};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use tauri::WebviewWindow;

    /// `NSWindowStyleMaskNonactivatingPanel`.
    const NONACTIVATING_PANEL: usize = 1 << 7;
    /// tao's window class keeps this one ivar; the panel class declares it
    /// too so the object layout (and tao's `set_focusable`) stay valid.
    const TAO_IVAR: &std::ffi::CStr = c"focusable";

    static KEYABLE: AtomicBool = AtomicBool::new(false);
    static CONVERTED: AtomicBool = AtomicBool::new(false);

    extern "C-unwind" fn can_become_key(_this: &AnyObject, _sel: Sel) -> Bool {
        Bool::new(KEYABLE.load(Ordering::Relaxed))
    }

    extern "C-unwind" fn can_become_main(_this: &AnyObject, _sel: Sel) -> Bool {
        Bool::NO
    }

    /// `ParleyRecorderPanel`: an NSPanel that becomes key only while typing.
    pub(super) fn panel_class() -> Option<&'static AnyClass> {
        static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
        *CLASS.get_or_init(|| {
            if let Some(existing) = AnyClass::get(c"ParleyRecorderPanel") {
                return Some(existing);
            }
            let mut builder =
                ClassBuilder::new(c"ParleyRecorderPanel", AnyClass::get(c"NSPanel")?)?;
            builder.add_ivar::<Bool>(TAO_IVAR);
            // SAFETY: the signatures match the selectors (`-(BOOL)…`).
            unsafe {
                builder.add_method(
                    sel!(canBecomeKeyWindow),
                    can_become_key as extern "C-unwind" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(canBecomeMainWindow),
                    can_become_main as extern "C-unwind" fn(_, _) -> _,
                );
            }
            Some(builder.register())
        })
    }

    /// The panel class can stand in for `class` only if the object layout is
    /// identical; otherwise the window stays a regular one.
    pub(super) fn same_layout(class: &AnyClass, panel: &AnyClass) -> bool {
        let offset = |c: &AnyClass| c.instance_variable(TAO_IVAR).map(|i| i.offset());
        class.instance_size() == panel.instance_size() && offset(class) == offset(panel)
    }

    pub(super) fn is_panel() -> bool {
        CONVERTED.load(Ordering::Relaxed)
    }

    pub(super) fn convert(window: &WebviewWindow) {
        let ns_window = match window.ns_window() {
            Ok(w) => w as usize,
            Err(e) => {
                log::warn!("[widget] no native window: {e}");
                return;
            }
        };
        let result = window.run_on_main_thread(move || {
            // SAFETY: on the main thread, with the live NSWindow of the widget.
            match unsafe { convert_on_main(ns_window as *mut AnyObject) } {
                Ok(()) => log::info!("[widget] recorder is a non-activating panel"),
                Err(e) => log::warn!("[widget] recorder stays a regular window: {e}"),
            }
        });
        if let Err(e) = result {
            log::warn!("[widget] {e}");
        }
    }

    unsafe fn convert_on_main(ns_window: *mut AnyObject) -> Result<(), String> {
        let window = unsafe { ns_window.as_ref() }.ok_or("null window")?;
        let panel = panel_class().ok_or("can't declare the panel class")?;
        let current = window.class();
        if !std::ptr::eq(current, panel) {
            if !same_layout(current, panel) {
                return Err(format!(
                    "{:?} doesn't have NSPanel's layout",
                    current.name()
                ));
            }
            // SAFETY: same instance size and ivar layout (checked above), and
            // NSPanel is an NSWindow subclass.
            unsafe { AnyObject::set_class(window, panel) };
        }
        unsafe {
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
            let _: () = msg_send![window, setHidesOnDeactivate: Bool::NO];
            // The style mask alone doesn't update the window server once the
            // window exists; this (private, so guarded) call does.
            let prevents: Bool =
                msg_send![window, respondsToSelector: sel!(_setPreventsActivation:)];
            if prevents.as_bool() {
                let _: () = msg_send![window, _setPreventsActivation: Bool::YES];
            }
        }
        CONVERTED.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub(super) fn set_keyboard(window: &WebviewWindow, on: bool) -> Result<(), String> {
        KEYABLE.store(on, Ordering::Relaxed);
        window
            .with_webview(move |webview| {
                let ns_window = webview.ns_window() as *mut AnyObject;
                let view = webview.inner() as *mut AnyObject;
                // SAFETY: the closure runs on the main thread with live objects.
                unsafe {
                    let Some(w) = ns_window.as_ref() else {
                        return;
                    };
                    if on {
                        let _: () = msg_send![w, makeKeyWindow];
                        if let Some(view) = view.as_ref() {
                            let _: Bool = msg_send![w, makeFirstResponder: view];
                        }
                        return;
                    }
                    let key: Bool = msg_send![w, isKeyWindow];
                    if key.as_bool() {
                        // Ordering the panel out hands the keyboard back to the
                        // app that is still active (the call); then show it again.
                        let visible: Bool = msg_send![w, isVisible];
                        let _: () = msg_send![w, orderOut: std::ptr::null_mut::<AnyObject>()];
                        if visible.as_bool() {
                            let _: () = msg_send![w, orderFrontRegardless];
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_note_field_grows_the_window() {
        assert_eq!(InputMode::None.height(), COMPACT_HEIGHT);
        assert_eq!(InputMode::Title.height(), COMPACT_HEIGHT);
        assert!(InputMode::Note.height() > COMPACT_HEIGHT);
        assert!(!InputMode::None.wants_keyboard());
        assert!(InputMode::Title.wants_keyboard() && InputMode::Note.wants_keyboard());
        let parsed: InputMode = serde_json::from_value(serde_json::json!("note")).unwrap();
        assert_eq!(parsed, InputMode::Note);
        assert!(serde_json::from_value::<InputMode>(serde_json::json!("focus")).is_err());
        assert!(serde_json::from_value::<InputMode>(serde_json::Value::Null).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn panel_class_is_an_nspanel_with_the_tao_window_layout() {
        use objc2_06::runtime::{AnyClass, ClassBuilder};
        let panel = panel::panel_class().expect("panel class");
        assert_eq!(panel.superclass().unwrap().name(), c"NSPanel");
        // Registering is idempotent.
        assert!(std::ptr::eq(panel, panel::panel_class().unwrap()));
        // A class shaped like tao's window (NSWindow + one BOOL ivar) matches;
        // one with an extra ivar doesn't.
        let mut like_tao =
            ClassBuilder::new(c"ParleyTestTaoWindow", AnyClass::get(c"NSWindow").unwrap()).unwrap();
        like_tao.add_ivar::<objc2_06::runtime::Bool>(c"focusable");
        let like_tao = like_tao.register();
        assert!(panel::same_layout(like_tao, panel));
        let mut bigger =
            ClassBuilder::new(c"ParleyTestBigWindow", AnyClass::get(c"NSWindow").unwrap()).unwrap();
        bigger.add_ivar::<objc2_06::runtime::Bool>(c"focusable");
        bigger.add_ivar::<u64>(c"extra");
        assert!(!panel::same_layout(bigger.register(), panel));
    }
}
