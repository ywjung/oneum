//! Keys of a keyboard on an Android phone, as egui events.
//!
//! The Java side hands over the keys that are the app's (not the input method's) with
//! Android's key codes (`platform::android::text_input`); here they become the key
//! events the shortcuts are read from. Nothing of Android's is needed for that, so it
//! is tested on every system.
use crate::platform::typed::HardKey;
use eframe::egui::{Event, Key, Modifiers};

/// The egui key for one of Android's `KeyEvent.KEYCODE_…` values, for the keys the app
/// asks for (see `InputView.forTheApp` in `android/`).
fn key_of(code: i32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    Some(match code {
        7..=16 => DIGITS[(code - 7) as usize],
        19 => Key::ArrowUp,
        20 => Key::ArrowDown,
        21 => Key::ArrowLeft,
        22 => Key::ArrowRight,
        29..=54 => LETTERS[(code - 29) as usize],
        55 => Key::Comma,
        56 => Key::Period,
        62 => Key::Space,
        66 | 160 => Key::Enter,
        67 => Key::Backspace,
        111 => Key::Escape,
        112 => Key::Delete,
        _ => return None,
    })
}

/// The key event for `key`; none for a key the app has no use for.
pub(super) fn event(key: &HardKey) -> Option<Event> {
    Some(Event::Key {
        key: key_of(key.code)?,
        physical_key: None,
        pressed: key.pressed,
        repeat: key.repeat,
        modifiers: Modifiers {
            alt: key.alt,
            ctrl: key.ctrl,
            shift: key.shift,
            mac_cmd: false,
            // What the shortcuts call "command" is Ctrl away from a Mac.
            command: key.ctrl,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(code: i32, meta: i32) -> Option<(Key, Modifiers)> {
        match event(&HardKey::from_android(code, meta, true, false))? {
            Event::Key {
                key,
                modifiers,
                pressed: true,
                repeat: false,
                ..
            } => Some((key, modifiers)),
            other => panic!("not a key press: {other:?}"),
        }
    }

    #[test]
    fn android_key_codes_become_the_keys_of_the_shortcuts() {
        // KEYCODE_SPACE, KEYCODE_ESCAPE, KEYCODE_A, KEYCODE_Z, KEYCODE_0, KEYCODE_9.
        assert_eq!(pressed(62, 0), Some((Key::Space, Modifiers::NONE)));
        assert_eq!(pressed(111, 0), Some((Key::Escape, Modifiers::NONE)));
        assert_eq!(pressed(29, 0).map(|(key, _)| key), Some(Key::A));
        assert_eq!(pressed(54, 0).map(|(key, _)| key), Some(Key::Z));
        assert_eq!(pressed(7, 0).map(|(key, _)| key), Some(Key::Num0));
        assert_eq!(pressed(16, 0).map(|(key, _)| key), Some(Key::Num9));
        // KEYCODE_ENTER and the one of the number pad.
        assert_eq!(pressed(66, 0).map(|(key, _)| key), Some(Key::Enter));
        assert_eq!(pressed(160, 0).map(|(key, _)| key), Some(Key::Enter));
        // A key the app has no use for (KEYCODE_TAB) and one outside the table.
        assert_eq!(pressed(61, 0), None);
        assert_eq!(pressed(9999, 0), None);
    }

    #[test]
    fn ctrl_is_the_command_key_and_the_other_modifiers_are_kept() {
        // META_CTRL_ON with KEYCODE_DPAD_RIGHT: "next song".
        let (key, modifiers) = pressed(22, 0x1000).unwrap();
        assert_eq!(key, Key::ArrowRight);
        assert!(modifiers.ctrl && modifiers.command && !modifiers.shift && !modifiers.alt);
        assert!(modifiers.matches_logically(Modifiers::COMMAND));
        // META_CTRL_ON | META_SHIFT_ON with KEYCODE_PERIOD: "stop after this song".
        let (key, modifiers) = pressed(56, 0x1001).unwrap();
        assert_eq!(key, Key::Period);
        assert!(modifiers.matches_logically(Modifiers::COMMAND | Modifiers::SHIFT));
        // META_CTRL_ON | META_ALT_ON with KEYCODE_DPAD_LEFT: "back five seconds".
        let (_, modifiers) = pressed(21, 0x1002).unwrap();
        assert!(modifiers.matches_logically(Modifiers::COMMAND | Modifiers::ALT));
        // Letting go and repeating are passed on as they are.
        let released = event(&HardKey::from_android(62, 0, false, false));
        assert!(matches!(released, Some(Event::Key { pressed: false, .. })));
        let held = event(&HardKey::from_android(22, 0x1000, true, true));
        assert!(matches!(held, Some(Event::Key { repeat: true, .. })));
    }
}
