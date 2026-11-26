//! A saved search ("smart playlist") as the conditions it is made of, for an editor
//! that sets them with buttons instead of having them typed.
//!
//! What is kept of a smart playlist stays the search as text (`genre:jazz rating:>=4
//! sort:plays limit:50`, see `search`): [`Rules::of`] reads such a text into the
//! conditions the editor has a control for, and [`Rules::query`] writes them back.
//! Whatever the editor has no control for (plain words, other fields, a value it
//! cannot show) stays as text in [`Rules::words`], so that nothing a person typed is
//! lost by opening the editor.
use super::search::yes_or_no;
use super::search::{FIELD_NAMES, Field, Subject, normalize, sort_order, time_span, year_range};
use super::snapshot::SortKey;

/// The stars a search asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stars {
    #[default]
    Any,
    /// This many or more (1 to 5).
    AtLeast(u8),
    /// Songs without stars.
    Unrated,
}

/// How often a song was played, as a search asks for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Plays {
    #[default]
    Any,
    /// Never played.
    Never,
    /// This often or more (1 and up).
    AtLeast(u32),
}

/// The orders the editor offers, by the names a search knows them by.
pub const ORDERS: [(SortKey, &str); 10] = [
    (SortKey::Title, "title"),
    (SortKey::Artist, "artist"),
    (SortKey::Album, "album"),
    (SortKey::Year, "year"),
    (SortKey::Duration, "length"),
    (SortKey::PlayCount, "plays"),
    (SortKey::DateAdded, "added"),
    (SortKey::LastPlayed, "played"),
    (SortKey::Rating, "rating"),
    (SortKey::Format, "format"),
];

/// A search as its conditions (see the module).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// Plain words, and every part of the search the editor has no control for, as
    /// they were typed.
    pub words: String,
    pub artist: String,
    pub genre: String,
    /// The first and the last year, either or both.
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    pub stars: Stars,
    /// In the favourites (`true`), or not in them.
    pub favorite: Option<bool>,
    pub plays: Plays,
    /// Added to the library within this many days.
    pub added_days: Option<u32>,
    /// Played within this many days.
    pub played_days: Option<u32>,
    /// The order, and whether it is turned round (`sort:-plays`).
    pub order: Option<(SortKey, bool)>,
    /// How many songs at most.
    pub limit: Option<usize>,
}

/// The parts of a search as they were typed: split at whitespace, with what stands
/// between double quotes kept together (quotes included).
fn raw_tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for c in query.chars() {
        match c {
            '"' | '“' | '”' => {
                quoted = !quoted;
                token.push('"');
            }
            c if (c.is_whitespace() || c.is_control()) && !quoted => {
                if !token.is_empty() {
                    out.push(std::mem::take(&mut token));
                }
            }
            c if c.is_control() => token.push(' '),
            c => token.push(c),
        }
    }
    if !token.is_empty() {
        out.push(token);
    }
    out
}

/// A value as it goes into a search: without quotes of its own, and between quotes
/// where it holds a space.
fn quoted(value: &str) -> String {
    let clean: String = value
        .chars()
        .filter(|c| !matches!(c, '"' | '“' | '”') && !c.is_control())
        .collect();
    let clean = clean.trim();
    if clean.chars().any(char::is_whitespace) {
        format!("\"{clean}\"")
    } else {
        clean.to_string()
    }
}

/// A year the editor can show.
fn shown_year(year: i32) -> Option<u16> {
    u16::try_from(year).ok().filter(|year| *year <= 9999)
}

impl Rules {
    /// Reads a search into its conditions. A part the editor has no control for, one
    /// whose value it cannot show, and a second condition on a field that has one
    /// already stay in [`Rules::words`] as they were typed. (An order and a number of
    /// songs are no conditions: of two, the later one counts, as in the search.)
    pub fn of(query: &str) -> Self {
        let mut rules = Rules::default();
        let mut rest: Vec<String> = Vec::new();
        for token in raw_tokens(query) {
            let named = token.split_once(':').and_then(|(name, value)| {
                let name = normalize(name);
                FIELD_NAMES
                    .iter()
                    .find(|(known, _)| *known == name)
                    .map(|(_, subject)| (*subject, value))
            });
            let taken = named.is_some_and(|(subject, value)| {
                let text: String = value.chars().filter(|c| *c != '"').collect();
                let text = text.trim();
                rules.take(subject, text)
            });
            if !taken {
                rest.push(token);
            }
        }
        rules.words = rest.join(" ");
        rules
    }

    /// Takes one `name:value` part in, if there is a control for it that is still
    /// unset and can show the value. Says whether it did.
    fn take(&mut self, subject: Subject, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let value = normalize(text);
        match subject {
            Subject::Text(Field::Artist) if self.artist.is_empty() => {
                self.artist = text.to_string();
                true
            }
            Subject::Text(Field::Genre) if self.genre.is_empty() => {
                self.genre = text.to_string();
                true
            }
            Subject::Year if self.year_from.is_none() && self.year_to.is_none() => {
                let Some((from, to)) = year_range(&value) else {
                    return false;
                };
                let from = (from != i32::MIN).then_some(from);
                let to = (to != i32::MAX).then_some(to);
                let shown = |year: Option<i32>| match year {
                    Some(year) => shown_year(year).map(Some),
                    None => Some(None),
                };
                match (shown(from), shown(to)) {
                    (Some(from), Some(to)) if from.is_some() || to.is_some() => {
                        (self.year_from, self.year_to) = (from, to);
                        true
                    }
                    _ => false,
                }
            }
            Subject::Rating if self.stars == Stars::Any => {
                self.stars = match year_range(&value) {
                    Some((least @ 1..=5, i32::MAX)) => Stars::AtLeast(least as u8),
                    Some((5, 5)) => Stars::AtLeast(5),
                    Some((i32::MIN | 0, 0)) => Stars::Unrated,
                    _ => return false,
                };
                true
            }
            Subject::Favorite if self.favorite.is_none() => {
                self.favorite = yes_or_no(&value);
                self.favorite.is_some()
            }
            Subject::Plays if self.plays == Plays::Any => {
                self.plays = match year_range(&value) {
                    Some((i32::MIN | 0, 0)) => Plays::Never,
                    Some((least @ 1.., i32::MAX)) => Plays::AtLeast(least as u32),
                    _ => return false,
                };
                true
            }
            Subject::Added if self.added_days.is_none() => {
                self.added_days = time_span(&value).map(|secs| (secs / 86_400) as u32);
                self.added_days.is_some()
            }
            Subject::Played if self.played_days.is_none() => {
                self.played_days = time_span(&value).map(|secs| (secs / 86_400) as u32);
                self.played_days.is_some()
            }
            // (Of two orders or two numbers the search goes by the later one: so does
            // this, and the earlier one, which asked for nothing any more, goes.)
            Subject::Sort => {
                let Some((key, _)) = sort_order(&value) else {
                    return false;
                };
                if !ORDERS.iter().any(|(known, _)| *known == key) {
                    return false;
                }
                self.order = Some((key, value.starts_with('-')));
                true
            }
            Subject::Limit => {
                let Some(most) = value.parse::<usize>().ok().filter(|most| *most > 0) else {
                    return false;
                };
                self.limit = Some(most);
                true
            }
            _ => false,
        }
    }

    /// The search these conditions make, as text (see `search`).
    pub fn query(&self) -> String {
        // The plain words first, as one types a search; what else the words hold
        // (conditions on fields) after the conditions of the controls, so that of two
        // conditions on one field the one a control shows stays the first, and is the
        // one the control shows when the search is read again.
        let (named, plain): (Vec<String>, Vec<String>) =
            raw_tokens(&self.words).into_iter().partition(|token| {
                token.split_once(':').is_some_and(|(name, _)| {
                    let name = normalize(name);
                    FIELD_NAMES.iter().any(|(known, _)| *known == name)
                })
            });
        let mut parts = plain;
        for (name, value) in [("artist", &self.artist), ("genre", &self.genre)] {
            let value = quoted(value);
            if !value.is_empty() {
                parts.push(format!("{name}:{value}"));
            }
        }
        match (self.year_from, self.year_to) {
            (Some(from), Some(to)) if from == to => parts.push(format!("year:{from}")),
            // (Typed the wrong way round: the span is meant all the same.)
            (Some(from), Some(to)) => {
                parts.push(format!("year:{}-{}", from.min(to), from.max(to)));
            }
            (Some(from), None) => parts.push(format!("year:>={from}")),
            (None, Some(to)) => parts.push(format!("year:<={to}")),
            (None, None) => {}
        }
        match self.stars {
            Stars::Any => {}
            Stars::AtLeast(least) => parts.push(format!("rating:>={}", least.clamp(1, 5))),
            Stars::Unrated => parts.push("rating:0".into()),
        }
        match self.favorite {
            Some(true) => parts.push("fav:yes".into()),
            Some(false) => parts.push("fav:no".into()),
            None => {}
        }
        match self.plays {
            Plays::Any => {}
            Plays::Never => parts.push("plays:0".into()),
            Plays::AtLeast(least) => parts.push(format!("plays:>={}", least.max(1))),
        }
        if let Some(days) = self.added_days.filter(|days| *days > 0) {
            parts.push(format!("added:{days}d"));
        }
        if let Some(days) = self.played_days.filter(|days| *days > 0) {
            parts.push(format!("played:{days}d"));
        }
        if let Some((key, turned)) = self.order
            && let Some((_, name)) = ORDERS.iter().find(|(known, _)| *known == key)
        {
            parts.push(format!("sort:{}{name}", if turned { "-" } else { "" }));
        }
        if let Some(most) = self.limit.filter(|most| *most > 0) {
            parts.push(format!("limit:{most}"));
        }
        parts.extend(named);
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::super::search::PreparedQuery;
    use super::*;

    const NOW: i64 = 1_800_000_000;

    /// The words, the conditions, the order and the number of songs of a search.
    type Asked = (
        Vec<String>,
        Vec<String>,
        Option<(SortKey, bool)>,
        Option<usize>,
    );

    /// What a search asks for, in a form in which two searches that ask for the same
    /// are equal: the conditions in one order (all of them have to hold, in whatever
    /// order they stand), and the numbers within what there is (no song has fewer than
    /// no stars or more than five, none was played fewer than no times).
    fn asked(query: &str) -> Asked {
        use super::super::search::Filter;
        let prepared = PreparedQuery::new_at(query, NOW);
        let mut filters: Vec<String> = prepared
            .filters
            .iter()
            .map(|filter| match filter {
                Filter::Rating(from, to) => {
                    format!("{:?}", Filter::Rating((*from).max(0), (*to).min(5)))
                }
                Filter::Plays(from, to) => format!("{:?}", Filter::Plays((*from).max(0), *to)),
                other => format!("{other:?}"),
            })
            .collect();
        filters.sort();
        let terms = prepared
            .terms
            .iter()
            .map(|term| term.text.clone())
            .collect();
        (terms, filters, prepared.order, prepared.limit)
    }

    #[test]
    fn a_search_is_read_into_its_conditions() {
        let rules = Rules::of(
            "가을 genre:\"hip hop\" artist:아이유 year:1990-1999 rating:>=4 fav:yes \
             plays:>=5 added:30d played:2w sort:-plays limit:50",
        );
        assert_eq!(
            rules,
            Rules {
                words: "가을".into(),
                artist: "아이유".into(),
                genre: "hip hop".into(),
                year_from: Some(1990),
                year_to: Some(1999),
                stars: Stars::AtLeast(4),
                favorite: Some(true),
                plays: Plays::AtLeast(5),
                added_days: Some(30),
                played_days: Some(14),
                order: Some((SortKey::PlayCount, true)),
                limit: Some(50),
            }
        );
        assert_eq!(
            rules.query(),
            "가을 artist:아이유 genre:\"hip hop\" year:1990-1999 rating:>=4 fav:yes \
             plays:>=5 added:30d played:14d sort:-plays limit:50"
        );
        // Plain words stay in front; other conditions follow those of the controls.
        assert_eq!(
            Rules::of("title:love 가을 genre:rock dr:>10 아침").query(),
            "가을 아침 genre:rock title:love dr:>10"
        );
        // The names in Korean, and other ways to write the same values.
        let rules = Rules::of(
            "장르:재즈 별점:5 즐겨찾기:아니오 재생:0 연도:>1999 정렬:별점 개수:25 추가:1년",
        );
        assert_eq!(
            rules,
            Rules {
                genre: "재즈".into(),
                year_from: Some(2000),
                stars: Stars::AtLeast(5),
                favorite: Some(false),
                plays: Plays::Never,
                added_days: Some(365),
                order: Some((SortKey::Rating, false)),
                limit: Some(25),
                ..Rules::default()
            }
        );
        assert_eq!(
            rules.query(),
            "genre:재즈 year:>=2000 rating:>=5 fav:no plays:0 added:365d sort:rating limit:25"
        );
        assert_eq!(Rules::of("rating:0").stars, Stars::Unrated);
        assert_eq!(Rules::of("year:<=1980").year_to, Some(1980));
        assert_eq!(Rules::of("year:1994").query(), "year:1994");
        assert_eq!(Rules::of("  ").query(), "");
    }

    /// What the editor has no control for stays in the words, as it was typed, and is
    /// part of the search as before.
    #[test]
    fn what_there_is_no_control_for_stays_as_it_was_typed() {
        for (query, words) in [
            // Plain words, quoted ones, other fields.
            (
                "\"Let It Be\" title:love album:\"Abbey Road\" format:lossless",
                "\"Let It Be\" title:love album:\"Abbey Road\" format:lossless",
            ),
            // Values the controls cannot show.
            ("rating:2-3", "rating:2-3"),
            ("plays:3-7", "plays:3-7"),
            ("year:abc", "year:abc"),
            ("year:12000-", "year:12000-"),
            ("limit:0", "limit:0"),
            ("limit:many", "limit:many"),
            ("sort:colour", "sort:colour"),
            ("added:soon", "added:soon"),
            ("fav:maybe", "fav:maybe"),
            ("dr:>10 check:ok", "dr:>10 check:ok"),
            // Still being typed, and a name that is none.
            ("genre:", "genre:"),
            ("mood:calm", "mood:calm"),
            // A second condition on a field that has one.
            ("genre:jazz genre:blues", "genre:blues"),
            ("year:1990..1995 year:2000", "year:2000"),
            // Of two orders or numbers the later one counts; one that is none stays.
            ("limit:10 limit:20", ""),
            ("sort:plays sort:-title", ""),
            ("limit:10 limit:many", "limit:many"),
            (
                "love genre:rock mood:calm dr:>10 가을",
                "love mood:calm dr:>10 가을",
            ),
        ] {
            let rules = Rules::of(query);
            assert_eq!(rules.words, words, "{query}");
            assert_eq!(
                asked(&rules.query()),
                asked(query),
                "{query} → {}",
                rules.query()
            );
        }
    }

    /// Reading a search and writing it again asks for the same songs in the same
    /// order, and what was written reads back as the same conditions.
    #[test]
    fn reading_and_writing_a_search_changes_nothing_it_asks_for() {
        for query in [
            "",
            "love",
            "가을 아침 artist:\"the beatles\" genre:rock",
            "genre:\"hip hop\" rating:>=3 sort:plays limit:50",
            "장르:재즈 별점:5 즐겨찾기:예 정렬:-추가 개수:10",
            "year:1990-1999 fav:no plays:0 added:6m played:1y",
            "year:>2000 rating:>3 plays:>9 sort:-title",
            "year:<1980 rating:<1 plays:<1",
            "year:1990..1995 year:2000",
            "ARTIST:IU Genre:Ballad SORT:Rating",
            "rating:4- plays:2- limit:100 sort:length",
            "artist:\"a b\" \"c d\" e",
        ] {
            let rules = Rules::of(query);
            let written = rules.query();
            assert_eq!(asked(&written), asked(query), "{query} → {written}");
            assert_eq!(Rules::of(&written), rules, "{query} → {written}");
            assert_eq!(Rules::of(&written).query(), written, "{query}");
        }
    }

    /// Conditions set with the controls are what the search then asks for.
    #[test]
    fn conditions_that_are_set_are_asked_for() {
        let rules = Rules {
            genre: " hip  \"hop\" ".into(),
            year_from: Some(2005),
            year_to: Some(1995),
            stars: Stars::AtLeast(9),
            plays: Plays::AtLeast(0),
            added_days: Some(0),
            limit: Some(0),
            order: Some((SortKey::TrackNo, false)),
            ..Rules::default()
        };
        // Quotes of the value's own go, a span the wrong way round is turned, what is
        // out of range is brought in, and what asks for nothing is left out.
        assert_eq!(
            rules.query(),
            "genre:\"hip  hop\" year:1995-2005 rating:>=5 plays:>=1"
        );
        let asked = PreparedQuery::new_at(&rules.query(), NOW);
        assert_eq!(asked.filters.len(), 4);
        assert!(asked.order.is_none() && asked.limit.is_none());
        let rules = Rules {
            order: Some((SortKey::LastPlayed, true)),
            limit: Some(20),
            ..Rules::default()
        };
        let asked = PreparedQuery::new_at(&rules.query(), NOW);
        assert_eq!(asked.order, Some((SortKey::LastPlayed, true)));
        assert_eq!(asked.limit, Some(20));
    }
}
