//! Text from the on-screen keyboard. An input method composes text (Korean syllables
//! out of letters) and changes what it has already written; winit only shows and hides
//! the keyboard and passes on neither. A view of the activity keeps what the input
//! method holds and reports it whole after every change (`changed`); the difference
//! from what was reported before is what was typed: letters to take back and letters
//! to add. The app drains that as key presses and text (`drain`).
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::platform::typed::difference;
pub use crate::platform::typed::{HardKey, Typed};

struct State {
    /// What the input method held when it last reported.
    held: String,
    typed: Vec<Typed>,
}

static STATE: Mutex<State> = Mutex::new(State {
    held: String::new(),
    typed: Vec::new(),
});
/// Keys of a keyboard (not of the on-screen one) that the app has not looked at yet.
static KEYS: Mutex<Vec<HardKey>> = Mutex::new(Vec::new());
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The keyboard was put away by the user while a field had it.
static DISMISSED: AtomicBool = AtomicBool::new(false);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What was typed since the last call.
pub fn drain() -> Vec<Typed> {
    std::mem::take(&mut state().typed)
}

/// The keys of a keyboard pressed or let go of since the last call. The Java side
/// sends them here instead of to the input method while no field is typed into, and
/// those with Ctrl (and Escape) always: they are the app's shortcuts.
pub fn drain_keys() -> Vec<HardKey> {
    std::mem::take(
        &mut *KEYS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// A key of a keyboard (see `natives`).
pub(super) fn hard_key(key: HardKey) {
    KEYS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(key);
}

/// A text field has the keyboard's attention (`true`) or none has. The keyboard comes
/// up or goes away, and the input method starts from nothing.
pub fn set_active(active: bool) {
    if ACTIVE.swap(active, Ordering::AcqRel) == active {
        return;
    }
    DISMISSED.store(false, Ordering::Release);
    state().held.clear();
    super::java::show_keyboard(active);
}

/// The keyboard left the screen. If a field was being typed into, it was the user who
/// sent the keyboard away: the field gives up its attention (see [`take_dismissed`]),
/// so that a tap on it brings the keyboard back.
pub(super) fn keyboard_went() {
    if ACTIVE.load(Ordering::Acquire) {
        DISMISSED.store(true, Ordering::Release);
    }
}

/// True once after the user put the keyboard away while a field had it.
pub fn take_dismissed() -> bool {
    DISMISSED.swap(false, Ordering::AcqRel)
}

/// The field was changed by something other than the keyboard (a tap moved the caret,
/// a button emptied it): what the input method holds no longer stands for the end of
/// the field's text, so it starts from nothing, and so does what it was composing.
pub fn reset() {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let had = {
        let mut state = state();
        !std::mem::take(&mut state.held).is_empty()
    };
    if had {
        super::java::reset_keyboard();
    }
}

/// The input method now holds `now`: what differs from before becomes key presses.
pub(super) fn changed(now: &str) {
    let mut state = state();
    let typed = difference(&state.held, now);
    state.typed.extend(typed);
    state.held = now.to_string();
}

/// A key that is not text: 1 delete backwards (with nothing held), 2 enter.
pub(super) fn key(code: i32) {
    let mut state = state();
    match code {
        1 => state.typed.push(Typed::Backspace),
        2 => {
            state.typed.push(Typed::Enter);
            // The field is done with: the Java side starts from nothing as well.
            state.held.clear();
        }
        _ => {}
    }
}
