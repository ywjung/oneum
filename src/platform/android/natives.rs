//! Calls from Java into Rust: the `native` methods of the class `Native` in `android/`.
//! They arrive on Java's main thread (or a binder thread), never on the app's own, so
//! each only notes what happened and wakes the app.
use super::{app_hooks, lifecycle, nowplaying, sound, text_input, window};
use crate::platform::SoundEvent;
use jni::EnvUnowned;
use jni::errors::LogErrorAndDefault;
use jni::objects::{JClass, JString};
use jni::sys::{jfloat, jint, jlong};
use std::path::PathBuf;

/// The text of a Java string argument; empty if it cannot be read.
fn text<'caller>(env: &mut EnvUnowned<'caller>, string: &JString<'caller>) -> String {
    env.with_env(|env| -> jni::errors::Result<String> { string.try_to_string(env) })
        .resolve::<LogErrorAndDefault>()
}

/// Paths that Java joined with line breaks.
fn paths(joined: &str) -> Vec<PathBuf> {
    joined
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// What the system's bars, the camera cut-out and the keyboard cover, in points.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onInsets<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    top: jfloat,
    left: jfloat,
    bottom: jfloat,
    right: jfloat,
    keyboard: jfloat,
) {
    window::set_insets([top, left, bottom, right, keyboard]);
    app_hooks::wake();
}

/// The phone's theme: dark (1) or light (0).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onDark<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    dark: jint,
) {
    window::set_system_dark(dark != 0);
    app_hooks::wake();
}

/// The activity came to the screen (1) or left it (0).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onForeground<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    foreground: jint,
) {
    if foreground != 0 {
        lifecycle::entering_foreground();
    } else {
        lifecycle::entered_background();
    }
}

/// The text the input method holds for the field that is being typed into.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onText<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    held: JString<'caller>,
) {
    let held = text(&mut env, &held);
    text_input::changed(&held);
    app_hooks::wake();
}

/// A key of the on-screen keyboard that is not text: 1 delete backwards, 2 enter.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onKey<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    key: jint,
) {
    text_input::key(key);
    app_hooks::wake();
}

/// A key of a keyboard that is plugged in or paired (not the on-screen one): its
/// Android key code and meta state, pressed or let go of, repeating or not.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onHardKey<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    code: jint,
    meta: jint,
    pressed: jint,
    repeat: jint,
) {
    text_input::hard_key(text_input::HardKey::from_android(
        code,
        meta,
        pressed != 0,
        repeat != 0,
    ));
    app_hooks::wake();
}

/// A button of the notification, the lock screen or a headset: 0 play, 1 pause,
/// 2 play or pause, 3 stop, 4 next, 5 previous, 6 seek to `argument` milliseconds.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onMedia<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    command: jint,
    argument: jlong,
) {
    nowplaying::remote(command, argument);
}

/// The system took the sound away or gave it back: 1 taken, 2 given back (play on),
/// 3 given back (stay silent), 4 the output is gone (headphones pulled out).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onSound<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    event: jint,
) {
    let event = match event {
        1 => SoundEvent::Interrupted,
        2 => SoundEvent::InterruptionOver { resume: true },
        3 => SoundEvent::InterruptionOver { resume: false },
        4 => SoundEvent::OutputGone,
        _ => return,
    };
    sound::notify(event);
}

/// What the user chose in the picker asked for under `id` (paths, one a line; none
/// if they chose nothing).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onPicked<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    id: jint,
    chosen: JString<'caller>,
) {
    let chosen = text(&mut env, &chosen);
    super::files::picked(id, paths(&chosen));
    app_hooks::wake();
}

/// Files another app asked OnEum to open (paths, one a line).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onOpen<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    files: JString<'caller>,
) {
    let files = text(&mut env, &files);
    app_hooks::receive(paths(&files));
}

/// Something was chosen in the picker asked for under `id` that cannot be used (a
/// folder that cannot be read by a path, files that could not be taken in): whoever
/// asked is told, with nothing (a picker that was put away tells nobody).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onPickFailed<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    id: jint,
) {
    super::files::pick_failed(id);
    app_hooks::wake();
}

/// What became of the file offered under `id`: kept where the user wanted it (0), the
/// question put away (1), or not written (2).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onHandedOut<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    id: jint,
    outcome: jint,
) {
    use crate::platform::HandedOut;
    let outcome = match outcome {
        0 => HandedOut::Kept,
        1 => HandedOut::PutAway,
        _ => HandedOut::Failed,
    };
    super::files::handed_out(id, outcome);
    app_hooks::wake();
}

/// The activity is going: for good (swiped away; `finishing` 1), or to be made anew
/// by the system at once (0). What is kept is saved and the process ends, so that
/// the next start is a fresh one.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_oneum_android_Native_onDestroy<'caller>(
    _env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    finishing: jint,
) {
    super::entry::end(finishing != 0);
}
