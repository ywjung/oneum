//! Text normalisation and query preparation for library search.
//!
//! Policy (docs/UX.md, "검색"): text is normalised with NFKC and Unicode lowercase, so
//! case, full-width/half-width forms and NFC/NFD differences are ignored. A query is
//! split on whitespace and every term must occur as a substring. A term made only of
//! Hangul initial consonants (ㄱ…ㅎ) is matched against the "choseong" form of the
//! text, where every Hangul syllable is replaced by its initial consonant.
use unicode_normalization::UnicodeNormalization;

/// Separator between the fields (and records) of the search blobs. Normalised text never
/// contains control characters, so a query term cannot match across a field boundary.
pub(crate) const FIELD_SEP: char = '\u{1f}';

/// The 19 Hangul initial consonants as compatibility jamo, in syllable-block order.
const CHOSEONG: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

const SYLLABLE_FIRST: u32 = 0xAC00;
const SYLLABLE_LAST: u32 = 0xD7A3;
/// Number of syllables per initial consonant (21 vowels × 28 finals).
const SYLLABLES_PER_INITIAL: u32 = 588;

/// Normalises text for searching and sorting: NFKC, then lowercase.
///
/// Two additions keep the result usable as a search key:
/// - NFKC turns compatibility jamo (`ㄱ`, U+3131) into conjoining jamo (U+1100). Initial
///   consonants that are still unattached after composition are mapped back to
///   compatibility jamo, the form used by the choseong index and typed by users.
/// - Control characters become spaces, so the field separator can never appear.
pub fn normalize(text: &str) -> String {
    if text.is_ascii() {
        return text
            .chars()
            .map(|c| {
                if c.is_ascii_control() {
                    ' '
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.nfkc() {
        match c {
            '\u{1100}'..='\u{1112}' => out.push(CHOSEONG[(c as u32 - 0x1100) as usize]),
            c if c.is_control() => out.push(' '),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

fn is_choseong(c: char) -> bool {
    CHOSEONG.contains(&c)
}

fn is_hangul_syllable(c: char) -> bool {
    (SYLLABLE_FIRST..=SYLLABLE_LAST).contains(&(c as u32))
}

/// Choseong form of already-normalised text: Hangul syllables become their initial
/// consonant, everything else is kept. Returns `None` when the text has no Hangul at all
/// (then a choseong query cannot match it and nothing needs to be stored).
pub(crate) fn choseong_form(normalized: &str) -> Option<String> {
    if normalized.is_ascii()
        || !normalized
            .chars()
            .any(|c| is_hangul_syllable(c) || is_choseong(c))
    {
        return None;
    }
    Some(
        normalized
            .chars()
            .map(|c| {
                if is_hangul_syllable(c) {
                    CHOSEONG[((c as u32 - SYLLABLE_FIRST) / SYLLABLES_PER_INITIAL) as usize]
                } else {
                    c
                }
            })
            .collect(),
    )
}

/// One whitespace-separated part of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Term {
    pub(crate) text: String,
    /// The term consists only of initial consonants and is matched against the choseong
    /// form of the text.
    pub(crate) choseong: bool,
}

/// A query split into normalised terms, ready to be matched against many tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedQuery {
    pub(crate) terms: Vec<Term>,
}

impl PreparedQuery {
    /// Normalises `query` and splits it into terms.
    pub fn new(query: &str) -> Self {
        let normalized = normalize(query);
        let mut terms: Vec<Term> = normalized
            .split_whitespace()
            .map(|term| Term {
                text: term.to_string(),
                choseong: term.chars().all(is_choseong),
            })
            .collect();
        terms.dedup();
        PreparedQuery { terms }
    }

    /// True when the query has no terms (it then matches every track).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_lowercased() {
        assert_eq!(normalize("The BEATLES"), "the beatles");
    }

    #[test]
    fn full_width_and_half_width_fold() {
        assert_eq!(normalize("ＡＢＣ　１２３"), "abc 123");
        // Half-width katakana composes with its voicing mark.
        assert_eq!(normalize("ｶﾞ"), "ガ");
    }

    #[test]
    fn nfd_hangul_composes() {
        let nfd = "\u{110B}\u{1161}\u{110B}\u{1175}\u{110B}\u{1172}";
        assert_eq!(normalize(nfd), "아이유");
        assert_eq!(normalize("아이유"), "아이유");
    }

    #[test]
    fn unicode_lowercase() {
        assert_eq!(normalize("ÉCOLE Ünïcode"), "école ünïcode");
        assert_eq!(normalize("ΣΊΣΥΦΟΣ"), "σίσυφοσ");
    }

    #[test]
    fn compatibility_jamo_survive_normalisation() {
        // NFKC alone would turn these into conjoining jamo.
        assert_eq!(normalize("ㅇㅇㅇ"), "ㅇㅇㅇ");
        assert_eq!(normalize("ㄲㄸㅃㅆㅉ"), "ㄲㄸㅃㅆㅉ");
        // Half-width jamo fold to the same characters.
        assert_eq!(normalize("\u{FFA1}"), "ㄱ");
    }

    #[test]
    fn control_characters_cannot_reach_the_blob() {
        assert_eq!(normalize("a\u{1f}b\tc\u{0}"), "a b c ");
        assert_eq!(normalize("한\u{1f}글"), "한 글");
    }

    #[test]
    fn choseong_of_syllables() {
        assert_eq!(choseong_form("아이유").as_deref(), Some("ㅇㅇㅇ"));
        assert_eq!(choseong_form("방탄소년단").as_deref(), Some("ㅂㅌㅅㄴㄷ"));
        assert_eq!(choseong_form("까치 love").as_deref(), Some("ㄲㅊ love"));
        assert_eq!(choseong_form("힣").as_deref(), Some("ㅎ"));
        assert_eq!(choseong_form("가").as_deref(), Some("ㄱ"));
        assert_eq!(choseong_form("the beatles"), None);
        assert_eq!(choseong_form("日本語"), None);
        // Literal consonants are kept so they can still be found.
        assert_eq!(choseong_form("ㅋㅋㅋ").as_deref(), Some("ㅋㅋㅋ"));
    }

    #[test]
    fn query_terms() {
        let q = PreparedQuery::new("  The　ＢＥＡ  ㅇㅇㅇ ㅇ아 ");
        assert_eq!(
            q.terms,
            vec![
                Term {
                    text: "the".into(),
                    choseong: false
                },
                Term {
                    text: "bea".into(),
                    choseong: false
                },
                Term {
                    text: "ㅇㅇㅇ".into(),
                    choseong: true
                },
                Term {
                    text: "ㅇ아".into(),
                    choseong: false
                },
            ]
        );
        assert!(!q.is_empty());
        assert!(PreparedQuery::new("").is_empty());
        assert!(PreparedQuery::new(" \t　").is_empty());
    }

    #[test]
    fn vowels_and_final_clusters_are_not_choseong_terms() {
        assert!(!PreparedQuery::new("ㅏ").terms[0].choseong);
        assert!(!PreparedQuery::new("ㄳ").terms[0].choseong);
    }
}
