//! Helpers for verification runs only (used by the scripted UI channel, never in normal
//! use): real key events delivered through the application's event queue, so that menu
//! key equivalents, the responder chain and the system input method are exercised.
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSTextInputContext};
use objc2_core_graphics::{CGEvent, CGEventFlags};
use objc2_foundation::NSString;

/// Posts key-down and key-up for a virtual key code to this app. Returns false when the
/// app has no key window (key events would go nowhere).
pub fn post_key(key_code: u16, command: bool, shift: bool) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    if app.keyWindow().is_none() {
        return false;
    }
    let mut flags = CGEventFlags(0);
    if command {
        flags |= CGEventFlags::MaskCommand;
    }
    if shift {
        flags |= CGEventFlags::MaskShift;
    }
    for down in [true, false] {
        // Created from a CGEvent so the current keyboard layout fills in the characters,
        // exactly like a hardware key press.
        let Some(cg) = CGEvent::new_keyboard_event(None, key_code, down) else {
            return false;
        };
        CGEvent::set_flags(Some(&cg), flags);
        match NSEvent::eventWithCGEvent(&cg) {
            Some(event) => app.postEvent_atStart(&event, false),
            None => return false,
        }
    }
    true
}

/// Selects a keyboard input source (e.g. `com.apple.inputmethod.Korean.2SetKorean`).
pub fn select_input_source(id: &str) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(context) = NSTextInputContext::currentInputContext(mtm) else {
        return false;
    };
    context.setSelectedKeyboardInputSource(Some(&NSString::from_str(id)));
    true
}

pub fn current_input_source() -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    NSTextInputContext::currentInputContext(mtm)?
        .selectedKeyboardInputSource()
        .map(|s| s.to_string())
}
