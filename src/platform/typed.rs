//! What an on-screen keyboard typed, worked out from what its input method holds.
//!
//! An input method composes text (Korean syllables out of letters) and changes what it
//! has already written. On Android the app is told the whole of what the input method
//! holds after every change (`android::text_input`); the difference from what it held
//! before is what was typed: letters to take back and letters to add. That part needs
//! nothing of Android's and is kept here, where it is tested on every system.

/// What the keyboard did, in the order it did it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Text(String),
    Backspace,
    Enter,
}

/// A key of a keyboard that is plugged in or paired with the phone, as Android names it
/// (`KeyEvent.KEYCODE_…`), with the modifiers that were held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardKey {
    pub code: i32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub pressed: bool,
    /// The key is held down and repeats.
    pub repeat: bool,
}

impl HardKey {
    /// From what Android says of a key event: its code and its `getMetaState()`.
    pub(crate) fn from_android(code: i32, meta: i32, pressed: bool, repeat: bool) -> Self {
        // KeyEvent.META_SHIFT_ON, META_ALT_ON, META_CTRL_ON.
        const SHIFT: i32 = 0x1;
        const ALT: i32 = 0x2;
        const CTRL: i32 = 0x1000;
        Self {
            code,
            ctrl: meta & CTRL != 0,
            shift: meta & SHIFT != 0,
            alt: meta & ALT != 0,
            pressed,
            repeat,
        }
    }
}

/// The key presses that turn `before` into `now`: what follows their common beginning
/// is taken back letter by letter, then what is new is typed.
pub(crate) fn difference(before: &str, now: &str) -> Vec<Typed> {
    let same = before
        .chars()
        .zip(now.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let mut typed = vec![Typed::Backspace; before.chars().count() - same];
    let added: String = now.chars().skip(same).collect();
    // A line break is the enter key, not a letter.
    for (index, part) in added.split('\n').enumerate() {
        if index > 0 {
            typed.push(Typed::Enter);
        }
        if !part.is_empty() {
            typed.push(Typed::Text(part.to_string()));
        }
    }
    typed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composing_a_korean_syllable_takes_back_and_types_again() {
        // ㅅ → 사 → 살 → 사라 (the last consonant moves on to the next syllable).
        assert_eq!(difference("", "ㅅ"), [Typed::Text("ㅅ".into())]);
        assert_eq!(
            difference("ㅅ", "사"),
            [Typed::Backspace, Typed::Text("사".into())]
        );
        assert_eq!(
            difference("살", "사라"),
            [Typed::Backspace, Typed::Text("사라".into())]
        );
        assert_eq!(difference("사랑", "사랑 "), [Typed::Text(" ".into())]);
    }

    #[test]
    fn deleting_and_line_breaks() {
        assert_eq!(difference("abc", "ab"), [Typed::Backspace]);
        assert_eq!(
            difference("abc", ""),
            [Typed::Backspace, Typed::Backspace, Typed::Backspace]
        );
        assert_eq!(difference("abc", "abc"), []);
        assert_eq!(
            difference("a", "ab\nc"),
            [
                Typed::Text("b".into()),
                Typed::Enter,
                Typed::Text("c".into())
            ]
        );
    }
}
