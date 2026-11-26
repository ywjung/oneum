//! Text normalisation and query preparation for library search.
//!
//! Policy (docs/UX.md, "검색"): text is normalised with NFKC and Unicode lowercase, so
//! case, full-width/half-width forms and NFC/NFD differences are ignored. A query is
//! split on whitespace and every term must occur as a substring. A term made only of
//! Hangul initial consonants (ㄱ…ㅎ) is matched against the "choseong" form of the
//! text, where every Hangul syllable is replaced by its initial consonant.
use super::snapshot::SortKey;
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

/// The vowels of a syllable in block order, each as the keys it is typed with (a
/// compound vowel is two keys).
const MEDIAL_KEYS: [&str; 21] = [
    "ㅏ", "ㅐ", "ㅑ", "ㅒ", "ㅓ", "ㅔ", "ㅕ", "ㅖ", "ㅗ", "ㅗㅏ", "ㅗㅐ", "ㅗㅣ", "ㅛ", "ㅜ",
    "ㅜㅓ", "ㅜㅔ", "ㅜㅣ", "ㅠ", "ㅡ", "ㅡㅣ", "ㅣ",
];
/// The final consonants of a syllable in block order (the first: none), each as the
/// keys it is typed with.
const FINAL_KEYS: [&str; 28] = [
    "", "ㄱ", "ㄲ", "ㄱㅅ", "ㄴ", "ㄴㅈ", "ㄴㅎ", "ㄷ", "ㄹ", "ㄹㄱ", "ㄹㅁ", "ㄹㅂ", "ㄹㅅ",
    "ㄹㅌ", "ㄹㅍ", "ㄹㅎ", "ㅁ", "ㅂ", "ㅂㅅ", "ㅅ", "ㅆ", "ㅇ", "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ",
    "ㅎ",
];

/// Text as the keys it is typed with on a Korean keyboard, in lower case: every
/// syllable as its consonants and vowels one after the other, a compound vowel or
/// final as its two keys. What is typed where nothing puts the keys together into
/// syllables (a list that is typed into to find a song) arrives like this, and a
/// title in this form begins with it where the title begins with what was meant:
/// `ㅎㅏㄴ` is how `한여름` begins, and `하늘`.
pub fn typed_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if is_hangul_syllable(c) {
            let at = c as u32 - SYLLABLE_FIRST;
            out.push(CHOSEONG[(at / SYLLABLES_PER_INITIAL) as usize]);
            out.push_str(MEDIAL_KEYS[(at % SYLLABLES_PER_INITIAL / 28) as usize]);
            out.push_str(FINAL_KEYS[(at % 28) as usize]);
            continue;
        }
        // A compound vowel or final that came as one letter.
        let alone = match c {
            'ㅘ' => "ㅗㅏ",
            'ㅙ' => "ㅗㅐ",
            'ㅚ' => "ㅗㅣ",
            'ㅝ' => "ㅜㅓ",
            'ㅞ' => "ㅜㅔ",
            'ㅟ' => "ㅜㅣ",
            'ㅢ' => "ㅡㅣ",
            'ㄳ' => "ㄱㅅ",
            'ㄵ' => "ㄴㅈ",
            'ㄶ' => "ㄴㅎ",
            'ㄺ' => "ㄹㄱ",
            'ㄻ' => "ㄹㅁ",
            'ㄼ' => "ㄹㅂ",
            'ㄽ' => "ㄹㅅ",
            'ㄾ' => "ㄹㅌ",
            'ㄿ' => "ㄹㅍ",
            'ㅀ' => "ㄹㅎ",
            'ㅄ' => "ㅂㅅ",
            _ => "",
        };
        if alone.is_empty() {
            out.extend(c.to_lowercase());
        } else {
            out.push_str(alone);
        }
    }
    out
}

/// Whether `text` holds Korean letters (syllables, or consonants and vowels alone).
pub fn has_hangul(text: &str) -> bool {
    text.chars()
        .any(|c| is_hangul_syllable(c) || matches!(c, '\u{3131}'..='\u{3163}'))
}

/// One whitespace-separated part of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Term {
    pub(crate) text: String,
    /// The term consists only of initial consonants and is matched against the choseong
    /// form of the text.
    pub(crate) choseong: bool,
}

/// The part of a track a field term (`artist:iu`) looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Title,
    /// The artist or the album artist.
    Artist,
    Album,
    Genre,
    /// Codec or container ("flac", "mp3"); "lossless" stands for the lossless codecs.
    Format,
}

/// A condition on one field of a track, written `field:value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Filter {
    Text {
        field: Field,
        term: Term,
    },
    /// Years from the first to the second, both included.
    Year(i32, i32),
    /// Times played, from the first to the second, both included.
    Plays(i32, i32),
    /// In the favourites, or not.
    Favorite(bool),
    /// Stars given, from the first to the second, both included (0: not rated).
    Rating(i32, i32),
    /// Added to the library at or after this time (Unix seconds).
    AddedSince(i64),
    /// Last played at or after this time (Unix seconds).
    PlayedSince(i64),
    /// What the file check found (see `library::check`).
    Check(CheckWanted),
    /// DR value (rounded) from the first to the second, both included.
    Dr(i32, i32),
    /// Length in whole minutes (3:59 is 3) from the first to the second, both
    /// included. A song whose length is not known has none.
    Length(i32, i32),
    /// A value that cannot be understood (`year:abc`): nothing matches, which says more
    /// than silently showing everything.
    Never,
}

/// What a `name:value` term is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Subject {
    Text(Field),
    Year,
    Plays,
    Favorite,
    Rating,
    Added,
    Played,
    Check,
    Dr,
    Length,
    /// Not a condition: the order the songs that match are given in (`sort:plays`).
    Sort,
    /// Not a condition: how many of them, at most (`limit:50`).
    Limit,
}

/// A `check:` condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckWanted {
    Damaged,
    /// Verified or decoded without a fault.
    Intact,
    /// Matched the checksum the file carries.
    Verified,
    /// Not checked (yet, or since the file changed).
    Unchecked,
}

fn check_wanted(value: &str) -> Option<CheckWanted> {
    match value {
        "damaged" | "bad" | "손상" | "손상됨" => Some(CheckWanted::Damaged),
        "ok" | "intact" | "good" | "정상" => Some(CheckWanted::Intact),
        "verified" | "md5" | "확인" => Some(CheckWanted::Verified),
        "none" | "unchecked" | "no" | "미검사" | "안함" => Some(CheckWanted::Unchecked),
        _ => None,
    }
}

/// The names a field can be asked for by, in normalised form.
pub(super) const FIELD_NAMES: [(&str, Subject); 42] = [
    ("title", Subject::Text(Field::Title)),
    ("제목", Subject::Text(Field::Title)),
    ("artist", Subject::Text(Field::Artist)),
    ("아티스트", Subject::Text(Field::Artist)),
    ("가수", Subject::Text(Field::Artist)),
    ("album", Subject::Text(Field::Album)),
    ("앨범", Subject::Text(Field::Album)),
    ("genre", Subject::Text(Field::Genre)),
    ("장르", Subject::Text(Field::Genre)),
    ("format", Subject::Text(Field::Format)),
    ("codec", Subject::Text(Field::Format)),
    ("형식", Subject::Text(Field::Format)),
    ("year", Subject::Year),
    ("date", Subject::Year),
    ("연도", Subject::Year),
    ("년도", Subject::Year),
    ("plays", Subject::Plays),
    ("재생", Subject::Plays),
    ("재생횟수", Subject::Plays),
    ("fav", Subject::Favorite),
    ("favorite", Subject::Favorite),
    ("즐겨찾기", Subject::Favorite),
    ("rating", Subject::Rating),
    ("stars", Subject::Rating),
    ("별점", Subject::Rating),
    ("added", Subject::Added),
    ("추가", Subject::Added),
    ("played", Subject::Played),
    ("들은", Subject::Played),
    ("최근재생", Subject::Played),
    ("check", Subject::Check),
    ("검사", Subject::Check),
    ("dr", Subject::Dr),
    ("length", Subject::Length),
    ("duration", Subject::Length),
    ("길이", Subject::Length),
    ("sort", Subject::Sort),
    ("order", Subject::Sort),
    ("정렬", Subject::Sort),
    ("limit", Subject::Limit),
    ("개수", Subject::Limit),
    ("제한", Subject::Limit),
];

/// The order a `sort:` term asks for: a name the lists know a column by, with `-` in
/// front of it for the other way round. Without that, what one looks for first comes
/// first: the songs played most, added or heard last, rated highest; names and
/// numbers from their beginning.
pub(super) fn sort_order(value: &str) -> Option<(SortKey, bool)> {
    let (name, turned) = match value.strip_prefix('-') {
        Some(name) => (name, true),
        None => (value, false),
    };
    let key = match name {
        "title" | "제목" => SortKey::Title,
        "artist" | "아티스트" | "가수" => SortKey::Artist,
        "album" | "앨범" => SortKey::Album,
        "year" | "연도" | "년도" => SortKey::Year,
        "length" | "duration" | "길이" => SortKey::Duration,
        "plays" | "재생" | "재생횟수" => SortKey::PlayCount,
        "added" | "추가" => SortKey::DateAdded,
        "played" | "들은" | "최근재생" => SortKey::LastPlayed,
        "rating" | "stars" | "별점" => SortKey::Rating,
        "format" | "형식" => SortKey::Format,
        _ => return None,
    };
    let ascending = !matches!(
        key,
        SortKey::PlayCount | SortKey::DateAdded | SortKey::LastPlayed | SortKey::Rating
    );
    Some((key, ascending != turned))
}

/// Splits a normalised query at whitespace; double quotes keep words together
/// (`"let it be"`, `artist:"the beatles"`) and are not part of the result.
fn tokens(normalized: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for c in normalized.chars() {
        match c {
            '"' | '“' | '”' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !token.trim().is_empty() {
                    out.push(token.trim().to_string());
                }
                token.clear();
            }
            c => token.push(c),
        }
    }
    if !token.trim().is_empty() {
        out.push(token.trim().to_string());
    }
    out
}

/// The numbers a `year:` or `plays:` value asks for: `1994`, `1990-1999` (also `..`
/// and `~`), `1990-` (from then on), `>=2000`, `>2000`, `<1980`, `<=1980`.
pub(super) fn year_range(value: &str) -> Option<(i32, i32)> {
    let year = |text: &str| {
        text.trim()
            .parse::<i32>()
            .ok()
            .filter(|y| (0..=1_000_000).contains(y))
    };
    for (prefix, range) in [
        (">=", (|y| (y, i32::MAX)) as fn(i32) -> (i32, i32)),
        ("<=", |y| (i32::MIN, y)),
        (">", |y| (y.saturating_add(1), i32::MAX)),
        ("<", |y| (i32::MIN, y.saturating_sub(1))),
    ] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return year(rest).map(range);
        }
    }
    for separator in ["..", "-", "~"] {
        if let Some((from, to)) = value.split_once(separator) {
            let from = year(from)?;
            let to = if to.trim().is_empty() {
                i32::MAX
            } else {
                year(to)?
            };
            return (from <= to).then_some((from, to));
        }
    }
    year(value).map(|y| (y, y))
}

/// The minutes a `length:` value asks for: written like a `year:` value (`3-6`,
/// `>=10`, `<2`, `4`), each number with or without its unit (`10m`, `10min`, `10분`).
pub(super) fn minutes_range(value: &str) -> Option<(i32, i32)> {
    let mut bare = value.to_string();
    for unit in ["minutes", "minute", "mins", "min", "분", "m"] {
        bare = bare.replace(unit, "");
    }
    // (What is left is digits and the signs of a span; anything else was no unit.)
    if bare.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    year_range(&bare)
}

/// A yes or a no, in the ways people write it.
pub(super) fn yes_or_no(value: &str) -> Option<bool> {
    match value {
        "yes" | "y" | "true" | "1" | "on" | "예" | "네" | "응" => Some(true),
        "no" | "n" | "false" | "0" | "off" | "아니오" | "아니요" | "아니" => Some(false),
        _ => None,
    }
}

/// A span of time counted back from now: `30d`, `2w`, `6m`, `1y` (also `30일`, `2주`,
/// `6달`, `6개월`, `1년`); a bare number is days. In seconds.
pub(super) fn time_span(value: &str) -> Option<i64> {
    let digits = value.chars().take_while(char::is_ascii_digit).count();
    let number: i64 = value[..digits]
        .parse()
        .ok()
        .filter(|n| (1..=36_500).contains(n))?;
    let days = match value[digits..].trim() {
        "" | "d" | "day" | "days" | "일" => 1,
        "w" | "week" | "weeks" | "주" => 7,
        "m" | "month" | "months" | "달" | "개월" => 30,
        "y" | "year" | "years" | "년" => 365,
        _ => return None,
    };
    Some(number * days * 86_400)
}

fn term(text: &str) -> Term {
    Term {
        text: text.to_string(),
        choseong: text.chars().all(is_choseong),
    }
}

/// A query split into normalised terms, ready to be matched against many tracks.
///
/// Plain terms are looked for in the title, artist, album and album artist. A term
/// written `field:value` (docs/UX.md, "검색") only looks at that field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedQuery {
    pub(crate) terms: Vec<Term>,
    pub(crate) filters: Vec<Filter>,
    /// The order the query asks for (`sort:plays`): a column, and whether ascending.
    /// A list that the user has put in an order of their own keeps that.
    pub(crate) order: Option<(SortKey, bool)>,
    /// How many songs the query wants at most (`limit:50`), counted in its order.
    pub(crate) limit: Option<usize>,
}

impl PreparedQuery {
    /// Normalises `query` and splits it into terms. Spans of time (`added:30d`) are
    /// counted back from now.
    pub fn new(query: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Self::new_at(query, now)
    }

    /// Like [`PreparedQuery::new`] with the present moment given (Unix seconds).
    pub fn new_at(query: &str, now: i64) -> Self {
        let normalized = normalize(query);
        let mut terms: Vec<Term> = Vec::new();
        let mut filters: Vec<Filter> = Vec::new();
        let (mut order, mut limit) = (None, None);
        for token in tokens(&normalized) {
            let named = token.split_once(':').and_then(|(name, value)| {
                FIELD_NAMES
                    .iter()
                    .find(|(known, _)| *known == name)
                    .map(|(_, field)| (*field, value.trim()))
            });
            match named {
                // Still being typed ("artist:"): no condition yet.
                Some((_, "")) => {}
                Some((Subject::Text(field), value)) => filters.push(Filter::Text {
                    field,
                    term: term(value),
                }),
                Some((subject, value)) => {
                    let filter = match subject {
                        Subject::Year => year_range(value)
                            .filter(|(from, _)| *from <= 9999)
                            .map(|(from, to)| Filter::Year(from, to)),
                        Subject::Plays => {
                            year_range(value).map(|(from, to)| Filter::Plays(from, to))
                        }
                        Subject::Favorite => yes_or_no(value).map(Filter::Favorite),
                        Subject::Rating => {
                            year_range(value).map(|(from, to)| Filter::Rating(from, to))
                        }
                        Subject::Added => time_span(value)
                            .map(|span| Filter::AddedSince(now.saturating_sub(span))),
                        Subject::Played => time_span(value)
                            .map(|span| Filter::PlayedSince(now.saturating_sub(span))),
                        Subject::Check => check_wanted(value).map(Filter::Check),
                        Subject::Dr => year_range(value).map(|(from, to)| Filter::Dr(from, to)),
                        Subject::Length => {
                            minutes_range(value).map(|(from, to)| Filter::Length(from, to))
                        }
                        // An order and a number are no conditions; one that cannot
                        // be understood finds nothing, like any other.
                        Subject::Sort => match sort_order(value) {
                            Some(asked) => {
                                order = Some(asked);
                                continue;
                            }
                            None => None,
                        },
                        Subject::Limit => match value.parse::<usize>() {
                            Ok(most) if most > 0 => {
                                limit = Some(most);
                                continue;
                            }
                            _ => None,
                        },
                        Subject::Text(_) => None,
                    };
                    filters.push(filter.unwrap_or(Filter::Never));
                }
                None => terms.push(term(&token)),
            }
        }
        terms.dedup();
        filters.dedup();
        PreparedQuery {
            terms,
            filters,
            order,
            limit,
        }
    }

    /// True when the query asks for nothing (it then matches every track, in the
    /// library's own order).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.filters.is_empty()
            && self.order.is_none()
            && self.limit.is_none()
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

    #[test]
    fn quotes_keep_words_together() {
        let q = PreparedQuery::new("\"Let It Be\"  jude");
        let texts: Vec<&str> = q.terms.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["let it be", "jude"]);
        assert!(q.filters.is_empty());
        // An unfinished quote takes the rest.
        let q = PreparedQuery::new("\"let it");
        assert_eq!(q.terms[0].text, "let it");
    }

    #[test]
    fn field_terms_are_told_from_plain_ones() {
        let q = PreparedQuery::new("Artist:\"The Beatles\" 장르:ㄹ year:1965-1970 help");
        assert_eq!(q.terms.len(), 1);
        assert_eq!(q.terms[0].text, "help");
        assert_eq!(
            q.filters,
            [
                Filter::Text {
                    field: Field::Artist,
                    term: Term {
                        text: "the beatles".into(),
                        choseong: false
                    }
                },
                Filter::Text {
                    field: Field::Genre,
                    term: Term {
                        text: "ㄹ".into(),
                        choseong: true
                    }
                },
                Filter::Year(1965, 1970),
            ]
        );
        // The full-width colon of a Korean keyboard works too.
        assert_eq!(
            PreparedQuery::new("앨범：abbey").filters,
            PreparedQuery::new("album:abbey").filters
        );
        // A colon after anything else is just text ("Re:Zero", "10:15").
        let q = PreparedQuery::new("re:zero 10:15");
        assert!(q.filters.is_empty());
        assert_eq!(q.terms.len(), 2);
        // A field without a value yet is no condition.
        assert!(PreparedQuery::new("artist:").is_empty());
        assert!(!PreparedQuery::new("year:1999").is_empty());
    }

    #[test]
    fn years_can_be_ranges_and_comparisons() {
        assert_eq!(year_range("1994"), Some((1994, 1994)));
        assert_eq!(year_range("1990-1999"), Some((1990, 1999)));
        assert_eq!(year_range("1990..1999"), Some((1990, 1999)));
        assert_eq!(year_range("1990~1999"), Some((1990, 1999)));
        assert_eq!(year_range("1990-"), Some((1990, i32::MAX)));
        assert_eq!(year_range(">=2000"), Some((2000, i32::MAX)));
        assert_eq!(year_range(">2000"), Some((2001, i32::MAX)));
        assert_eq!(year_range("<1980"), Some((i32::MIN, 1979)));
        assert_eq!(year_range("<=1980"), Some((i32::MIN, 1980)));
        assert_eq!(year_range("1999-1990"), None);
        assert_eq!(year_range("abc"), None);
        assert_eq!(year_range(">"), None);
        assert_eq!(PreparedQuery::new("year:abc").filters, [Filter::Never]);
    }

    #[test]
    fn what_the_file_check_found_can_be_asked_for() {
        assert_eq!(
            PreparedQuery::new("check:damaged dr:<8").filters,
            [Filter::Check(CheckWanted::Damaged), Filter::Dr(i32::MIN, 7)]
        );
        assert_eq!(
            PreparedQuery::new("검사:정상 dr:10-14").filters,
            [Filter::Check(CheckWanted::Intact), Filter::Dr(10, 14)]
        );
        assert_eq!(
            PreparedQuery::new("check:md5").filters,
            [Filter::Check(CheckWanted::Verified)]
        );
        assert_eq!(
            PreparedQuery::new("검사:미검사").filters,
            [Filter::Check(CheckWanted::Unchecked)]
        );
        assert_eq!(PreparedQuery::new("check:maybe").filters, [Filter::Never]);
    }

    #[test]
    fn an_order_and_a_number_are_asked_for_beside_the_conditions() {
        let q = PreparedQuery::new_at("rating:>=3 sort:plays limit:25", 0);
        assert_eq!(q.filters, [Filter::Rating(3, i32::MAX)]);
        assert_eq!(
            q.order,
            Some((SortKey::PlayCount, false)),
            "most played first"
        );
        assert_eq!(q.limit, Some(25));
        // The other way round, and in Korean.
        assert_eq!(
            PreparedQuery::new_at("sort:-plays", 0).order,
            Some((SortKey::PlayCount, true))
        );
        assert_eq!(
            PreparedQuery::new_at("정렬:제목 개수:10", 0),
            PreparedQuery {
                order: Some((SortKey::Title, true)),
                limit: Some(10),
                ..PreparedQuery::default()
            }
        );
        assert_eq!(
            PreparedQuery::new_at("sort:-year", 0).order,
            Some((SortKey::Year, false))
        );
        // An order alone asks for something: every song, in that order.
        assert!(!PreparedQuery::new_at("sort:title", 0).is_empty());
        assert!(
            PreparedQuery::new_at("sort:", 0).is_empty(),
            "still being typed"
        );
        // What cannot be understood finds nothing.
        for bad in ["sort:colour", "limit:0", "limit:many", "limit:-3"] {
            assert_eq!(
                PreparedQuery::new_at(bad, 0).filters,
                [Filter::Never],
                "{bad}"
            );
        }
    }

    #[test]
    fn stars_are_asked_for_as_numbers() {
        assert_eq!(
            PreparedQuery::new_at("rating:>=4", 0).filters,
            [Filter::Rating(4, i32::MAX)]
        );
        assert_eq!(
            PreparedQuery::new_at("별점:5 stars:0", 0).filters,
            [Filter::Rating(5, 5), Filter::Rating(0, 0)]
        );
        assert_eq!(
            PreparedQuery::new_at("rating:many", 0).filters,
            [Filter::Never]
        );
    }

    #[test]
    fn what_the_library_keeps_about_a_song_can_be_asked_for() {
        const NOW: i64 = 1_800_000_000;
        let q = PreparedQuery::new_at("plays:>=5 fav:yes added:30d played:2w", NOW);
        assert!(q.terms.is_empty());
        assert_eq!(
            q.filters,
            [
                Filter::Plays(5, i32::MAX),
                Filter::Favorite(true),
                Filter::AddedSince(NOW - 30 * 86_400),
                Filter::PlayedSince(NOW - 14 * 86_400),
            ]
        );
        // Korean names and units.
        assert_eq!(
            PreparedQuery::new_at("재생:0 즐겨찾기:아니오 추가:6개월 들은:1년", NOW).filters,
            [
                Filter::Plays(0, 0),
                Filter::Favorite(false),
                Filter::AddedSince(NOW - 180 * 86_400),
                Filter::PlayedSince(NOW - 365 * 86_400),
            ]
        );
        assert_eq!(time_span("7"), Some(7 * 86_400));
        assert_eq!(time_span("3weeks"), Some(21 * 86_400));
        assert_eq!(time_span("0d"), None);
        assert_eq!(time_span("soon"), None);
        // What cannot be understood matches nothing.
        assert_eq!(
            PreparedQuery::new_at("fav:maybe added:soon plays:many", NOW).filters,
            [Filter::Never]
        );
        // A year stays a year.
        assert_eq!(
            PreparedQuery::new_at("year:123456", NOW).filters,
            [Filter::Never]
        );
    }
    #[test]
    fn text_as_the_keys_it_is_typed_with() {
        assert_eq!(typed_keys("한여름"), "ㅎㅏㄴㅇㅕㄹㅡㅁ");
        assert_eq!(typed_keys("하늘"), "ㅎㅏㄴㅡㄹ");
        // Compound vowels and finals are two keys; doubled consonants are one.
        assert_eq!(typed_keys("과"), "ㄱㅗㅏ");
        assert_eq!(typed_keys("값"), "ㄱㅏㅂㅅ");
        assert_eq!(typed_keys("꽃"), "ㄲㅗㅊ");
        assert_eq!(typed_keys("있"), "ㅇㅣㅆ");
        assert_eq!(typed_keys("의"), "ㅇㅡㅣ");
        assert_eq!(typed_keys("ㅘㄳ"), "ㅗㅏㄱㅅ");
        // The first and the last syllable there is.
        assert_eq!(typed_keys("가"), "ㄱㅏ");
        assert_eq!(typed_keys("힣"), "ㅎㅣㅎ");
        // Everything else is itself, in lower case.
        assert_eq!(typed_keys("Abbey Road 2 밤"), "abbey road 2 ㅂㅏㅁ");
        assert!(typed_keys("한여름밤").starts_with(&typed_keys("ㅎㅏㄴㅇ")));
        assert!(has_hangul("밤") && has_hangul("ㅂ") && has_hangul("ㅏ"));
        assert!(!has_hangul("Abbey Road") && !has_hangul("夜"));
    }
}
