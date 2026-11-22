//! Calls from Rust into Java: the methods of the app's activity (`MainActivity` in
//! `android/`). Each call attaches the calling thread to the Java machine if it is not
//! yet; what has to run on Java's main thread is sent there on the Java side. A call
//! that cannot be made or throws is logged and answers with nothing.
use jni::objects::{JObject, JString};
use jni::{Env, JValue, JavaVM, jni_sig, jni_str};
use std::sync::OnceLock;
use winit::platform::android::activity::AndroidApp;

type JniResult<T> = jni::errors::Result<T>;

/// The activity's handle, kept from the start of `android_main` for as long as the
/// process lives.
static APP: OnceLock<AndroidApp> = OnceLock::new();

pub(super) fn set_app(app: AndroidApp) {
    let _ = APP.set(app);
}

/// Runs `f` with this thread's Java environment and the app's activity.
fn with_activity<T>(
    what: &str,
    f: impl for<'local> FnOnce(&mut Env<'local>, &JObject<'local>) -> JniResult<T>,
) -> Option<T> {
    let app = APP.get()?;
    // SAFETY: the process's one Java machine, as the activity glue hands it out.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr();
    let result: JniResult<T> = vm.attach_current_thread(|env| {
        // SAFETY: a global reference to the activity, which the glue keeps for as long
        // as the activity exists; it is only borrowed for this call.
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        f(env, &activity)
    });
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!("a call into Java failed ({what}): {error}");
            None
        }
    }
}

/// A Java string as a Rust one (`None` for Java's `null`).
fn string_of<'local>(env: &mut Env<'local>, object: JObject<'local>) -> JniResult<Option<String>> {
    if object.is_null() {
        return Ok(None);
    }
    let string = env.cast_local::<JString>(object)?;
    Ok(Some(string.try_to_string(env)?))
}

/// The language the phone is set to ("ko", "en").
pub(super) fn language() -> Option<String> {
    with_activity("language", |env, activity| {
        let answer = env
            .call_method(
                activity,
                jni_str!("language"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        string_of(env, answer)
    })
    .flatten()
}

/// Bytes in the Korean code page 949 as text; `None` if they are not that.
pub(super) fn decode_cp949(bytes: &[u8]) -> Option<String> {
    with_activity("decodeKorean", |env, activity| {
        let array = env.byte_array_from_slice(bytes)?;
        let answer = env
            .call_method(
                activity,
                jni_str!("decodeKorean"),
                jni_sig!("([B)Ljava/lang/String;"),
                &[JValue::Object(&array)],
            )?
            .l()?;
        string_of(env, answer)
    })
    .flatten()
}

pub(super) fn open_web_page(address: &str) -> bool {
    with_activity("openWebPage", |env, activity| {
        let address = env.new_string(address)?;
        env.call_method(
            activity,
            jni_str!("openWebPage"),
            jni_sig!("(Ljava/lang/String;)Z"),
            &[JValue::Object(&address)],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Asks for the sound before any is made; true if it is the app's (see `sound::claim`).
/// True also when the question could not be put: the music is not held back for that.
pub(super) fn claim_sound() -> bool {
    with_activity("claimSound", |env, activity| {
        env.call_method(activity, jni_str!("claimSound"), jni_sig!("()Z"), &[])?
            .z()
    })
    .unwrap_or(true)
}

/// Tells the playback service how an interruption ended for the app.
pub(super) fn sound_settled(resumed: bool) {
    with_activity("soundSettled", |env, activity| {
        env.call_method(
            activity,
            jni_str!("soundSettled"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(resumed)],
        )?;
        Ok(())
    });
}

/// Shows the folder `path` in the system's file manager; false if that cannot be done.
pub(super) fn show_folder(path: &str) -> bool {
    with_activity("showFolder", |env, activity| {
        let path = env.new_string(path)?;
        env.call_method(
            activity,
            jni_str!("showFolder"),
            jni_sig!("(Ljava/lang/String;)Z"),
            &[JValue::Object(&path)],
        )?
        .z()
    })
    .unwrap_or(false)
}

pub(super) fn alert(title: &str, body: &str) {
    with_activity("alert", |env, activity| {
        let title = env.new_string(title)?;
        let body = env.new_string(body)?;
        env.call_method(
            activity,
            jni_str!("alert"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Object(&title), JValue::Object(&body)],
        )?;
        Ok(())
    });
}

/// A method of the activity that takes one truth value and answers nothing.
fn tell(name: &'static jni::strings::JNIStr, what: &str, value: bool) {
    with_activity(what, |env, activity| {
        env.call_method(activity, name, jni_sig!("(Z)V"), &[JValue::from(value)])?;
        Ok(())
    });
}

/// A method of the activity that takes and answers nothing.
fn ask(name: &'static jni::strings::JNIStr, what: &str) {
    with_activity(what, |env, activity| {
        env.call_method(activity, name, jni_sig!("()V"), &[])?;
        Ok(())
    });
}

/// How the app looks, for the icons of the system's bars: 0 as the phone is set,
/// 1 light, 2 dark.
pub(super) fn set_appearance(mode: i32) {
    with_activity("setAppearance", |env, activity| {
        env.call_method(
            activity,
            jni_str!("setAppearance"),
            jni_sig!("(I)V"),
            &[JValue::Int(mode)],
        )?;
        Ok(())
    });
}

pub(super) fn show_keyboard(show: bool) {
    tell(jni_str!("showKeyboard"), "showKeyboard", show);
}

/// Tells the input method that what is typed now is a password (or no longer is).
pub(super) fn secret_keyboard(secret: bool) {
    tell(jni_str!("secretKeyboard"), "secretKeyboard", secret);
}

/// Has the input method forget what it holds and what it was composing.
pub(super) fn reset_keyboard() {
    ask(jni_str!("resetKeyboard"), "resetKeyboard");
}

/// Has the app opened again once this process is gone (see `RelaunchActivity`).
/// False when that could not be asked for.
pub(super) fn relaunch() -> bool {
    with_activity("relaunch", |env, activity| {
        env.call_method(activity, jni_str!("relaunch"), jni_sig!("()V"), &[])?;
        Ok(())
    })
    .is_some()
}

/// Puts `text` on the system's clipboard. False when that could not be asked for.
pub(super) fn copy_text(text: &str) -> bool {
    with_activity("copyText", |env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            jni_str!("copyText"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&text)],
        )?;
        Ok(())
    })
    .is_some()
}

/// A line the system shows for a moment, also once the app is gone.
pub(super) fn notice(text: &str) {
    with_activity("notice", |env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            jni_str!("notice"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&text)],
        )?;
        Ok(())
    });
}

/// The accessibility tree, flattened (`ui::accessible`), for A11yHost.java.
pub(super) fn accessibility_tree(json: &str) {
    with_activity("accessibilityTree", |env, activity| {
        let json = env.new_string(json)?;
        env.call_method(
            activity,
            jni_str!("accessibilityTree"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&json)],
        )?;
        Ok(())
    });
}

/// For verification: asks A11yHost.java to do to a node what TalkBack would.
#[cfg(any(debug_assertions, feature = "automation"))]
pub(super) fn accessibility_ask(said: &str, what: i32) {
    with_activity("accessibilityAsk", |env, activity| {
        let said = env.new_string(said)?;
        env.call_method(
            activity,
            jni_str!("accessibilityAsk"),
            jni_sig!("(Ljava/lang/String;I)V"),
            &[JValue::Object(&said), JValue::Int(what)],
        )?;
        Ok(())
    });
}

pub(super) fn move_to_back() {
    ask(jni_str!("moveToBack"), "moveToBack");
}

/// Whether the app may read all of shared storage ("all files access").
pub(super) fn has_all_files_access() -> bool {
    with_activity("hasAllFilesAccess", |env, activity| {
        env.call_method(
            activity,
            jni_str!("hasAllFilesAccess"),
            jni_sig!("()Z"),
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Shows the system's page where that right is given.
pub(super) fn ask_all_files_access() {
    ask(jni_str!("askAllFilesAccess"), "askAllFilesAccess");
}

/// Whether music can be read by its path: the system's right to music and audio, or
/// all files access.
pub(super) fn has_music_access() -> bool {
    with_activity("hasMusicAccess", |env, activity| {
        env.call_method(activity, jni_str!("hasMusicAccess"), jni_sig!("()Z"), &[])?
            .z()
    })
    .unwrap_or(false)
}

/// Asks for the right to music and audio (the system's dialog, or the app's page of
/// its settings where the system no longer asks).
pub(super) fn ask_music_access() {
    ask(jni_str!("askMusicAccess"), "askMusicAccess");
}

/// Shows the system's picker: `kind` 0 a folder, 1 files, 2 one file. The answer comes
/// to `natives` under `id`.
pub(super) fn pick(kind: i32, id: i32) {
    with_activity("pick", |env, activity| {
        env.call_method(
            activity,
            jni_str!("pick"),
            jni_sig!("(II)V"),
            &[JValue::Int(kind), JValue::Int(id)],
        )?;
        Ok(())
    });
}

/// Asks where to keep the file at `path` (suggesting `name`) and copies it there; what
/// became of it is told under `id` (`Native.onHandedOut`). False if Java could not be
/// asked.
pub(super) fn hand_out(path: &str, name: &str, id: i32) -> bool {
    with_activity("handOut", |env, activity| {
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        env.call_method(
            activity,
            jni_str!("handOut"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;I)V"),
            &[
                JValue::Object(&path),
                JValue::Object(&name),
                JValue::Int(id),
            ],
        )?;
        Ok(())
    })
    .is_some()
}

/// What plays, for the notification and the lock screen. Times in milliseconds.
pub(super) fn update_playback(info: &super::nowplaying::NowPlayingInfo) {
    let duration_ms = (info.duration_s * 1000.0) as i64;
    let position_ms = (info.position_s * 1000.0) as i64;
    let (playing, speed) = (info.playing, info.rate as f32);
    let art_path = info
        .art_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    with_activity("updatePlayback", |env, activity| {
        let title = env.new_string(&info.title)?;
        let artist = env.new_string(&info.artist)?;
        let album = env.new_string(&info.album)?;
        let art_path = env.new_string(&art_path)?;
        env.call_method(
            activity,
            jni_str!("updatePlayback"),
            jni_sig!(
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJZFLjava/lang/String;)V"
            ),
            &[
                JValue::Object(&title),
                JValue::Object(&artist),
                JValue::Object(&album),
                JValue::Long(duration_ms),
                JValue::Long(position_ms),
                JValue::from(playing),
                JValue::Float(speed),
                JValue::Object(&art_path),
            ],
        )?;
        Ok(())
    });
}

/// Nothing plays any more: the notification goes.
pub(super) fn stop_playback() {
    ask(jni_str!("stopPlayback"), "stopPlayback");
}

/// Keeps a password under `id` (see `Secrets.java`). False when it could not be kept.
pub(super) fn secret_put(id: &str, secret: &str) -> bool {
    with_activity("secretPut", |env, activity| {
        let id = env.new_string(id)?;
        let secret = env.new_string(secret)?;
        env.call_method(
            activity,
            jni_str!("secretPut"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Z"),
            &[JValue::Object(&id), JValue::Object(&secret)],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// The password kept under `id`.
pub(super) fn secret_get(id: &str) -> Option<String> {
    with_activity("secretGet", |env, activity| {
        let id = env.new_string(id)?;
        let answer = env
            .call_method(
                activity,
                jni_str!("secretGet"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&id)],
            )?
            .l()?;
        string_of(env, answer)
    })
    .flatten()
}

pub(super) fn secret_remove(id: &str) {
    with_activity("secretRemove", |env, activity| {
        let id = env.new_string(id)?;
        env.call_method(
            activity,
            jni_str!("secretRemove"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&id)],
        )?;
        Ok(())
    });
}
