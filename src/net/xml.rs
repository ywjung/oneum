//! A small XML reader: enough for what media servers send (device descriptions, SOAP
//! envelopes, DIDL-Lite) and no more. It hands out one event at a time and checks only
//! what it needs to keep its place: tags must be closed and must match, nothing else
//! is validated.
//!
//! The text comes from the network, so nothing in it is trusted. A `<!DOCTYPE …>` is
//! passed over together with what it declares, and only the five predefined entities
//! and character references are ever replaced: an entity that stands for a thousand
//! others stays the few bytes it was written as. Nesting and attributes are limited,
//! and the reader never calls itself, however the document is nested.

/// Elements nested deeper than this are not a document a media server sends.
const MAX_DEPTH: usize = 64;
/// More attributes than any element of these documents has.
const MAX_ATTRIBUTES: usize = 64;
/// The longest reference looked for after an `&` (`&#x10FFFF;` with room for leading
/// zeros). Anything longer is ordinary text.
const MAX_REFERENCE_BYTES: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// An element begins. The name is as written (with its prefix), the attribute
    /// values are unescaped. An element written `<a/>` gives `Start` and then `End`.
    Start {
        name: String,
        attrs: Vec<(String, String)>,
    },
    End {
        name: String,
    },
    /// The text between two tags, unescaped, with what CDATA sections held. Text of
    /// nothing but white space is not reported.
    Text(String),
}

/// What was wrong with a document.
#[derive(Debug, Clone, PartialEq)]
pub struct XmlError(pub &'static str);

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for XmlError {}

pub struct Reader<'a> {
    /// What has not been read yet.
    rest: &'a str,
    /// The names of the elements that are open, outermost first.
    open: Vec<&'a str>,
    /// The element last reported was written `<a/>`: its end is reported next.
    self_closed: bool,
    done: bool,
}

impl<'a> Reader<'a> {
    pub fn new(text: &'a str) -> Self {
        Reader {
            // A byte order mark is not part of the document.
            rest: text.strip_prefix('\u{feff}').unwrap_or(text),
            open: Vec::new(),
            self_closed: false,
            done: false,
        }
    }

    /// `None` at the end; `Some(Err)` once on malformed input, then `None`.
    pub fn next_event(&mut self) -> Option<Result<Event, XmlError>> {
        if self.done {
            return None;
        }
        if self.self_closed {
            self.self_closed = false;
            if let Some(name) = self.open.pop() {
                return Some(Ok(Event::End {
                    name: name.to_string(),
                }));
            }
        }
        match self.read() {
            Ok(Some(event)) => Some(Ok(event)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }

    /// The next event, `None` at the end of the document.
    fn read(&mut self) -> Result<Option<Event>, XmlError> {
        // Text is gathered across comments and CDATA sections, so that one element's
        // text arrives as one event however it was written.
        let mut text = String::new();
        loop {
            let rest = self.rest;
            // Text outside the outermost element is passed over rather than refused:
            // some servers end their documents with a NUL byte or a stray line.
            let inside = !self.open.is_empty();
            if rest.is_empty() {
                return if inside {
                    Err(XmlError("the document ends inside an element"))
                } else {
                    Ok(None)
                };
            }
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                if inside {
                    unescape_into(&rest[..end], &mut text);
                }
                self.rest = &rest[end..];
            } else if let Some(after) = rest.strip_prefix("<!--") {
                self.rest = after_end(after, "-->", "a comment is not closed")?;
            } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
                let end = after
                    .find("]]>")
                    .ok_or(XmlError("a CDATA section is not closed"))?;
                if inside {
                    text.push_str(&after[..end]);
                }
                self.rest = &after[end + 3..];
            } else if let Some(after) = rest.strip_prefix("<?") {
                self.rest = after_end(after, "?>", "a processing instruction is not closed")?;
            } else if let Some(after) = strip_doctype(rest) {
                self.rest = skip_doctype(after)?;
            } else if text.chars().any(|c| !c.is_whitespace()) {
                // A tag follows; it is read by the next call.
                return Ok(Some(Event::Text(text)));
            } else {
                return self.tag().map(Some);
            }
        }
    }

    /// Reads the start or end tag `rest` begins with.
    fn tag(&mut self) -> Result<Event, XmlError> {
        let after = self.rest.strip_prefix('<').unwrap_or(self.rest);
        if let Some(after) = after.strip_prefix('/') {
            let end = after
                .find('>')
                .ok_or(XmlError("an end tag is not closed"))?;
            let name = after[..end].trim_end();
            match self.open.pop() {
                Some(open) if open == name => {}
                Some(_) => return Err(XmlError("an end tag does not match its start tag")),
                None => return Err(XmlError("an end tag without a start tag")),
            }
            self.rest = &after[end + 1..];
            return Ok(Event::End {
                name: name.to_string(),
            });
        }
        let name_end = after
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .ok_or(XmlError("a tag is not closed"))?;
        let name = &after[..name_end];
        if !is_name(name) {
            return Err(XmlError("not an element name"));
        }
        let mut rest = &after[name_end..];
        let mut attrs = Vec::new();
        let self_closed = loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("/>") {
                rest = after;
                break true;
            }
            if let Some(after) = rest.strip_prefix('>') {
                rest = after;
                break false;
            }
            if attrs.len() == MAX_ATTRIBUTES {
                return Err(XmlError("an element has too many attributes"));
            }
            let (attribute, after) = attribute(rest)?;
            attrs.push(attribute);
            rest = after;
        };
        if self.open.len() == MAX_DEPTH {
            return Err(XmlError("elements are nested too deep"));
        }
        self.open.push(name);
        self.self_closed = self_closed;
        self.rest = rest;
        Ok(Event::Start {
            name: name.to_string(),
            attrs,
        })
    }
}

/// What follows the first `end` in `text`.
fn after_end<'a>(text: &'a str, end: &str, error: &'static str) -> Result<&'a str, XmlError> {
    let at = text.find(end).ok_or(XmlError(error))?;
    Ok(&text[at + end.len()..])
}

/// What follows `<!DOCTYPE`, when `text` begins with it.
fn strip_doctype(text: &str) -> Option<&str> {
    const KEYWORD: &str = "<!DOCTYPE";
    let start = text.get(..KEYWORD.len())?;
    start
        .eq_ignore_ascii_case(KEYWORD)
        .then(|| &text[KEYWORD.len()..])
}

/// Passes over a document type declaration (`text` begins after `<!DOCTYPE`) and
/// returns what follows it. The declaration ends at the first `>` outside its internal
/// subset (`[ … ]`); quoted strings and comments may hold any of these characters.
/// Nothing declared in it is kept, so no entity of the document's own is ever known.
fn skip_doctype(text: &str) -> Result<&str, XmlError> {
    const UNCLOSED: XmlError = XmlError("a document type declaration is not closed");
    let mut rest = text;
    let mut in_subset = false;
    loop {
        let at = rest.find(['"', '\'', '[', ']', '<', '>']).ok_or(UNCLOSED)?;
        rest = &rest[at..];
        let mut chars = rest.chars();
        let found = chars.next().ok_or(UNCLOSED)?;
        let after = chars.as_str();
        rest = match found {
            '"' | '\'' => {
                let close = after.find(found).ok_or(UNCLOSED)?;
                &after[close + 1..]
            }
            '<' => match after.strip_prefix("!--") {
                Some(comment) => after_end(comment, "-->", UNCLOSED.0)?,
                None => after,
            },
            '[' => {
                in_subset = true;
                after
            }
            ']' => {
                in_subset = false;
                after
            }
            // Inside the subset a '>' ends one of its declarations.
            _ if in_subset => after,
            _ => return Ok(after),
        };
    }
}

/// One `name="value"` (or `name='value'`) at the start of `text`, and what follows it.
fn attribute(text: &str) -> Result<((String, String), &str), XmlError> {
    let name_end = text
        .find(|c: char| c.is_whitespace() || c == '=')
        .ok_or(XmlError("a tag is not closed"))?;
    let name = &text[..name_end];
    if !is_name(name) {
        return Err(XmlError("not an attribute name"));
    }
    let after = text[name_end..]
        .trim_start()
        .strip_prefix('=')
        .ok_or(XmlError("an attribute without a value"))?
        .trim_start();
    let mut chars = after.chars();
    let quote = match chars.next() {
        Some(quote @ ('"' | '\'')) => quote,
        _ => return Err(XmlError("an attribute value without quotes")),
    };
    let value = chars.as_str();
    let end = value
        .find(quote)
        .ok_or(XmlError("an attribute value is not closed"))?;
    Ok((
        (name.to_string(), unescape(&value[..end])),
        &value[end + 1..],
    ))
}

/// Whether `name` can be the name of an element or an attribute. Looser than the XML
/// grammar (any letter or digit of any script), strict enough that markup characters
/// never end up in a name.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == ':')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
}

/// The name without its namespace prefix ("dc:title" -> "title").
pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Replaces the five predefined entities and numeric character references; anything
/// else stays as written.
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    unescape_into(text, &mut out);
    out
}

fn unescape_into(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match reference(after) {
            Some((character, len)) => {
                out.push(character);
                rest = &after[len..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
}

/// The character the reference at the start of `text` (what follows an `&`) stands
/// for, and how many bytes the reference takes, its `;` included.
fn reference(text: &str) -> Option<(char, usize)> {
    let end = text
        .bytes()
        .take(MAX_REFERENCE_BYTES)
        .position(|byte| byte == b';')?;
    let name = text.get(..end)?;
    let character = match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let (digits, radix) = match number.strip_prefix(['x', 'X']) {
                Some(hex) => (hex, 16),
                None => (number, 10),
            };
            // `from_str_radix` would also take a sign.
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            let code = u32::from_str_radix(digits, radix).ok()?;
            // NUL is no character of a document, and text with one in it ends early
            // wherever it is handed to the system.
            char::from_u32(code).filter(|&c| c != '\0')?
        }
    };
    Some((character, end + 1))
}

/// For putting text into a request.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Vec<Result<Event, XmlError>> {
        let mut reader = Reader::new(text);
        let mut events = Vec::new();
        while let Some(event) = reader.next_event() {
            events.push(event);
        }
        events
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Result<Event, XmlError> {
        Ok(Event::Start {
            name: name.to_string(),
            attrs: attrs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        })
    }

    fn end(name: &str) -> Result<Event, XmlError> {
        Ok(Event::End {
            name: name.to_string(),
        })
    }

    fn text(text: &str) -> Result<Event, XmlError> {
        Ok(Event::Text(text.to_string()))
    }

    #[test]
    fn reads_elements_attributes_and_text() {
        assert_eq!(
            events("<?xml version=\"1.0\"?>\n<a x=\"1\" y='two'>\n  <b>hi</b>\n  <c/>\n</a>\n"),
            vec![
                start("a", &[("x", "1"), ("y", "two")]),
                start("b", &[]),
                text("hi"),
                end("b"),
                start("c", &[]),
                end("c"),
                end("a"),
            ]
        );
    }

    #[test]
    fn text_is_one_event_across_cdata_and_comments() {
        assert_eq!(
            events("<t>AC<![CDATA[/]]>DC <!-- live --> &amp; more</t>"),
            vec![start("t", &[]), text("AC/DC  & more"), end("t")]
        );
        // What CDATA holds is not unescaped, and markup in it is text.
        assert_eq!(
            events("<t><![CDATA[<b>&amp;</b>]]></t>"),
            vec![start("t", &[]), text("<b>&amp;</b>"), end("t")]
        );
    }

    #[test]
    fn unescape_replaces_only_what_xml_defines() {
        assert_eq!(
            unescape("&lt;a&gt; &amp; &quot;b&quot; &apos;c&apos;"),
            "<a> & \"b\" 'c'"
        );
        assert_eq!(unescape("&#65;&#x42;&#X43;&#xD55C;"), "ABC한");
        assert_eq!(unescape("&amp;lt;"), "&lt;");
        // Unknown entities, references to no character and a lone '&' stay as written.
        for same in [
            "&nbsp;",
            "&lol9;",
            "&#0;",
            "&#xD800;",
            "&#x110000;",
            "&#+65;",
            "&#;",
            "&#x;",
            "&#99999999999;",
            "AT&T",
            "a & b",
            "&",
            "&amp",
            "&averyveryverylongname;",
        ] {
            assert_eq!(unescape(same), same);
        }
    }

    #[test]
    fn escape_and_unescape_are_inverse() {
        let plain = "Tom & Jerry <\"live\"> 'at' 서울";
        let escaped = escape(plain);
        assert_eq!(
            escaped,
            "Tom &amp; Jerry &lt;&quot;live&quot;&gt; &apos;at&apos; 서울"
        );
        assert_eq!(unescape(&escaped), plain);
    }

    #[test]
    fn local_drops_the_prefix() {
        assert_eq!(local("dc:title"), "title");
        assert_eq!(local("title"), "title");
        assert_eq!(local("a:b:c"), "c");
        assert_eq!(local(""), "");
    }

    #[test]
    fn the_end_of_a_doctype_is_found_past_its_subset() {
        assert_eq!(skip_doctype(" html>rest"), Ok("rest"));
        assert_eq!(
            skip_doctype(" a [ <!ENTITY b \"]>\"> <!-- ]> ' --> ]>rest"),
            Ok("rest")
        );
        assert!(skip_doctype(" a [ <!ENTITY b \"c\">").is_err());
        assert!(skip_doctype(" a \"unclosed>").is_err());
    }
}
