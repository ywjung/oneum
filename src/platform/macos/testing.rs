//! Helpers for verification runs only (used by the scripted UI channel, never in normal
//! use): real key events delivered through the application's event queue, so that menu
//! key equivalents, the responder chain and the system input method are exercised.
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSTextInputContext};
use objc2_core_graphics::{CGEvent, CGEventFlags};
use objc2_foundation::NSString;

/// Posts key-down and key-up for a virtual key code to this app. Returns false when the
/// app has no key window (key events would go nowhere).
pub fn post_key(key_code: u16, command: bool, shift: bool, option: bool) -> bool {
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
    if option {
        flags |= CGEventFlags::MaskAlternate;
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

/// Hands a key press to the menu bar the way the application does for a key the user
/// pressed while it is active: the menu looks for an item with that key equivalent and
/// carries out its action. Returns whether an item took it. This checks that a
/// shortcut is registered for the key as the keyboard reports it (layout, Shift and
/// Option included) and that it is wired to its command; it works while another app is
/// in front, which [`post_key`] does not.
pub fn menu_key(key_code: u16, command: bool, shift: bool, option: bool) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let mut flags = CGEventFlags(0);
    if command {
        flags |= CGEventFlags::MaskCommand;
    }
    if shift {
        flags |= CGEventFlags::MaskShift;
    }
    if option {
        flags |= CGEventFlags::MaskAlternate;
    }
    let Some(cg) = CGEvent::new_keyboard_event(None, key_code, true) else {
        return false;
    };
    CGEvent::set_flags(Some(&cg), flags);
    let (Some(event), Some(menu)) = (
        NSEvent::eventWithCGEvent(&cg),
        NSApplication::sharedApplication(mtm).mainMenu(),
    ) else {
        return false;
    };
    menu.performKeyEquivalent(&event)
}

/// True while this app is the active one (the one key presses go to). A key posted
/// with [`post_key`] only reaches the menu's key equivalents then.
pub fn app_is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
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
