//! Bridges the standard Edit-menu actions from the AppKit responder chain into egui.
//!
//! The native Edit menu sends `copy:`, `paste:` … to the first responder. winit's view
//! does not implement them, so AppKit swallows ⌘C/⌘V before egui sees the key press.
//! We add those methods to winit's view class at runtime and queue the action; the UI
//! drains the queue in `raw_input_hook` and turns it into egui events. Native text
//! fields (open panels) keep working because they handle the same selectors themselves.
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{ffi, msg_send, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Undo,
    Redo,
}

static QUEUE: Mutex<Vec<EditAction>> = Mutex::new(Vec::new());
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

fn push(action: EditAction) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(action);
    }
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

/// Takes all pending edit actions (called from the UI thread once per frame).
pub fn drain() -> Vec<EditAction> {
    QUEUE
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

macro_rules! handler {
    ($name:ident, $action:expr) => {
        extern "C-unwind" fn $name(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
            push($action);
        }
    };
}
handler!(on_cut, EditAction::Cut);
handler!(on_copy, EditAction::Copy);
handler!(on_paste, EditAction::Paste);
handler!(on_select_all, EditAction::SelectAll);
handler!(on_undo, EditAction::Undo);
handler!(on_redo, EditAction::Redo);

type Handler = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

static TEXT_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Tells the bridge whether an egui text field currently has keyboard focus.
/// While it does, ⌘←/⌘→/⌘↑/⌘↓ move the text cursor instead of triggering the menu
/// commands bound to the same keys.
pub fn set_text_input_active(active: bool) {
    TEXT_INPUT_ACTIVE.store(active, Ordering::Relaxed);
}

const KEY_LEFT: u16 = 123;
const KEY_UP: u16 = 126;

/// `-[NSView performKeyEquivalent:]` for winit's view. Views get the first chance at a
/// key equivalent, before the menu bar.
extern "C-unwind" fn perform_key_equivalent(
    this: *mut AnyObject,
    _cmd: Sel,
    event: *mut AnyObject,
) -> Bool {
    if event.is_null() || !TEXT_INPUT_ACTIVE.load(Ordering::Relaxed) {
        return Bool::NO;
    }
    // SAFETY: AppKit passes a valid `NSEvent *` that outlives this call.
    let event_ref: &NSEvent = unsafe { &*event.cast::<NSEvent>() };
    if event_ref.r#type() != NSEventType::KeyDown {
        return Bool::NO;
    }
    let flags = event_ref.modifierFlags();
    let plain_command = flags.contains(NSEventModifierFlags::Command)
        && !flags.contains(NSEventModifierFlags::Control)
        && !flags.contains(NSEventModifierFlags::Option);
    if plain_command && (KEY_LEFT..=KEY_UP).contains(&event_ref.keyCode()) {
        // SAFETY: `this` is the winit view receiving the event; `keyDown:` is implemented
        // by it and takes the same event object.
        unsafe {
            let _: () = msg_send![this, keyDown: event];
        }
        return Bool::YES;
    }
    Bool::NO
}

#[cfg(target_arch = "aarch64")]
const KEY_EQUIVALENT_TYPES: &std::ffi::CStr = c"B@:@";
#[cfg(not(target_arch = "aarch64"))]
const KEY_EQUIVALENT_TYPES: &std::ffi::CStr = c"c@:@";

/// Installs the edit-action methods on winit's view class. Must be called after the
/// window exists (the class is registered lazily). Returns false if the class is missing,
/// in which case the Edit menu shortcuts will not reach egui text fields.
pub fn install(waker: impl Fn() + Send + Sync + 'static) -> bool {
    let _ = WAKER.set(Box::new(waker));
    let Some(cls) = AnyClass::get(c"WinitView") else {
        return false;
    };
    let table: [(Sel, Handler); 6] = [
        (sel!(cut:), on_cut),
        (sel!(copy:), on_copy),
        (sel!(paste:), on_paste),
        (sel!(selectAll:), on_select_all),
        (sel!(undo:), on_undo),
        (sel!(redo:), on_redo),
    ];
    for (sel, f) in table {
        // SAFETY: `cls` is a registered class. The IMP has the signature `void (id, SEL, id)`
        // which matches the type encoding "v@:@" used by AppKit action methods. Adding a
        // method that the class does not yet implement does not affect existing behaviour;
        // `class_addMethod` is a no-op (returns NO) if it already exists.
        unsafe {
            let imp: Imp = std::mem::transmute::<Handler, Imp>(f);
            ffi::class_addMethod(
                std::ptr::from_ref(cls).cast_mut(),
                sel,
                imp,
                c"v@:@".as_ptr(),
            );
        }
    }
    type KeyEquivalent = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;
    // SAFETY: same as above; the IMP matches `BOOL (id, SEL, NSEvent *)`. winit's view does
    // not override this method, so NSView's default (which returns NO for a leaf view) is
    // what our implementation falls back to.
    unsafe {
        let imp: Imp = std::mem::transmute::<KeyEquivalent, Imp>(perform_key_equivalent);
        ffi::class_addMethod(
            std::ptr::from_ref(cls).cast_mut(),
            sel!(performKeyEquivalent:),
            imp,
            KEY_EQUIVALENT_TYPES.as_ptr(),
        );
    }
    true
}

/// Reads plain text from the general pasteboard (for `EditAction::Paste`).
pub fn pasteboard_text() -> Option<String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    let pb = NSPasteboard::generalPasteboard();
    // SAFETY: `NSPasteboardTypeString` is a valid static provided by AppKit.
    let s = pb.stringForType(unsafe { NSPasteboardTypeString })?;
    Some(s.to_string())
}
