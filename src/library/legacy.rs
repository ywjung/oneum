//! Korean text from before Unicode.
//!
//! Music files and lyric files that were tagged on Korean Windows years ago hold their
//! text in the system code page of the time (CP949, a superset of EUC-KR) without
//! saying so: an ID3 tag declares "Latin-1", a `.lrc` or `.m3u` file declares nothing.
//! Read as declared, "가사" comes out as "°¡»ç". This module recognises such text and
//! reads it as what it is.
//!
//! Recognising is strict, so that Western text with accents is left alone: every byte
//! above ASCII must be part of a well-formed two-byte character, and at least one of
//! those must be a Hangul syllable of the basic set (which takes two accented letters
//! in a row of a kind no Western word has: "°¡", "¾Æ"). The conversion itself is the
//! operating system's.

/// True if `bytes` are well-formed CP949 with at least one common Hangul syllable.
pub(crate) fn is_korean_cp949(bytes: &[u8]) -> bool {
    let mut hangul = false;
    let mut rest = bytes;
    while let [first, tail @ ..] = rest {
        if first.is_ascii() {
            rest = tail;
            continue;
        }
        let [second, tail @ ..] = tail else {
            return false; // a lead byte with nothing after it
        };
        let well_formed = match first {
            // The extension area: syllables added by CP949, trail bytes include letters.
            0x81..=0xC6 => matches!(second, 0x41..=0x5A | 0x61..=0x7A | 0x81..=0xFE),
            // KS X 1001 proper only.
            0xC7..=0xFE => matches!(second, 0xA1..=0xFE),
            _ => false,
        };
        if !well_formed {
            return false;
        }
        // The 2,350 syllables of KS X 1001: rows 0xB0 to 0xC8.
        hangul |= matches!(first, 0xB0..=0xC8) && *second >= 0xA1;
        rest = tail;
    }
    hangul
}

/// `bytes` read as CP949, if that is what they are.
fn from_cp949(bytes: &[u8]) -> Option<String> {
    if !is_korean_cp949(bytes) {
        return None;
    }
    let text = crate::platform::decode_cp949(bytes)?;
    let sound = !text.contains('\u{fffd}') && text.chars().any(|c| ('가'..='힣').contains(&c));
    sound.then_some(text)
}

/// The Korean text behind a string that was read as Latin-1 ("°¡»ç" → "가사"). `None`
/// for anything else, which is nearly everything: ASCII, real Unicode, Western text.
pub(crate) fn repair(text: &str) -> Option<String> {
    if text.is_ascii() {
        return None;
    }
    // Latin-1 is the first 256 code points: each character is the byte it came from.
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect::<Option<_>>()?;
    from_cp949(&bytes)
}

/// The text of a file that does not say how it is encoded: UTF-8 (with or without a
/// byte order mark), UTF-16 with one, or Korean in CP949. `None` if it is none of
/// these: it is then not shown rather than shown as garbage.
pub(crate) fn decode_file(bytes: &[u8]) -> Option<String> {
    let utf16 = |bytes: &[u8], big_endian: bool| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes(*pair)
                } else {
                    u16::from_le_bytes(*pair)
                }
            })
            .collect();
        String::from_utf16(&units).ok()
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        _ => String::from_utf8(bytes.to_vec())
            .ok()
            .or_else(|| from_cp949(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "가사" and "아이유" as CP949 bytes.
    const GASA: [u8; 4] = [0xB0, 0xA1, 0xBB, 0xE7];
    const IU: [u8; 6] = [0xBE, 0xC6, 0xC0, 0xCC, 0xC0, 0xAF];

    fn as_latin1(bytes: &[u8]) -> String {
        bytes.iter().map(|b| char::from(*b)).collect()
    }

    #[test]
    fn korean_in_the_old_code_page_is_recognised() {
        assert!(is_korean_cp949(&GASA));
        assert!(is_korean_cp949(&IU));
        // With ASCII around it, as in a file name or a title with a number.
        let mut mixed = b"01. ".to_vec();
        mixed.extend(IU);
        mixed.extend(b" - Track (Live).mp3");
        assert!(is_korean_cp949(&mixed));
        // A syllable from the extension area next to common ones.
        assert!(is_korean_cp949(&[0x8C, 0x63, 0xB0, 0xA1]));
        // Not it: plain ASCII, a lead byte at the end, a lead byte before a digit,
        // bytes that are not lead bytes.
        assert!(!is_korean_cp949(b"Plain title"));
        assert!(!is_korean_cp949(&[0xB0, 0xA1, 0xBB]));
        assert!(!is_korean_cp949(&[0xB0, 0x31]));
        assert!(!is_korean_cp949(&[0x80, 0xA1]));
        assert!(!is_korean_cp949(&[0xFF, 0xA1]));
        assert!(!is_korean_cp949(&[]));
    }

    #[test]
    fn western_text_with_accents_is_left_alone() {
        for title in [
            "Beyoncé",
            "Motörhead",
            "Sigur Rós",
            "Mötley Crüe",
            "Déjà Vu",
            "DÉJÀ VU",
            "Ändern",
            "Señor Coconut",
            "naïve",
            "¿Qué pasa?",
            "Ærø",
            "Âme",
            "Für Elise",
            "Café del Mar, Vol. 3",
            "Garçon",
            "Ó Paí, Ó",
            "Les Misérables: À la volonté du peuple",
        ] {
            assert_eq!(repair(title), None, "{title}");
        }
        // Real Unicode and plain ASCII are never touched.
        assert_eq!(repair("아이유 — 밤편지"), None);
        assert_eq!(repair("Plain title"), None);
        assert_eq!(repair(""), None);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn mojibake_from_a_latin1_tag_is_read_as_korean() {
        assert_eq!(repair(&as_latin1(&GASA)).as_deref(), Some("가사"));
        assert_eq!(repair("°¡»ç").as_deref(), Some("가사"));
        assert_eq!(repair(&as_latin1(&IU)).as_deref(), Some("아이유"));
        let mut mixed = b"01. ".to_vec();
        mixed.extend(IU);
        mixed.extend(b" (Live)");
        assert_eq!(
            repair(&as_latin1(&mixed)).as_deref(),
            Some("01. 아이유 (Live)")
        );
        // Repaired text needs no further repair.
        assert_eq!(repair("가사"), None);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn files_are_read_in_the_encoding_they_are_in() {
        assert_eq!(
            decode_file("유니코드".as_bytes()).as_deref(),
            Some("유니코드")
        );
        assert_eq!(
            decode_file(b"\xEF\xBB\xBFwith a mark").as_deref(),
            Some("with a mark")
        );
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("가사".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_file(&utf16).as_deref(), Some("가사"));
        let utf16_be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("가사".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode_file(&utf16_be).as_deref(), Some("가사"));
        let mut old: Vec<u8> = b"[00:01.00]".to_vec();
        old.extend(GASA);
        assert_eq!(decode_file(&old).as_deref(), Some("[00:01.00]가사"));
        // Neither Unicode nor Korean: not guessed at.
        assert_eq!(decode_file(&[0xE9, 0x20, 0xE8]), None);
        assert_eq!(decode_file(b"").as_deref(), Some(""));
    }
}
