//! What a media server holds, asked for the UPnP way. The server's description says
//! where its ContentDirectory service takes commands (`describe`); a `Browse` command
//! sent there returns the children of a folder (`browse`) as a DIDL-Lite document
//! (`parse_didl`), each song with the addresses it can be fetched from
//! (`best_resource` picks the one to play).
//!
//! Servers differ in what they fill in and how well they keep to the documents'
//! grammar. Whatever is missing or unreadable is left out: an entry without a title
//! shows its id, a number that is none is `None`, an address that is not `http://` is
//! not offered.
use super::http::{self, Limits, Url};
use super::xml::{self, Event, Reader};
use super::{NetError, capped};

/// A device description is a few kilobytes.
const MAX_DESCRIPTION_BYTES: u64 = 1 << 20;
/// A page of 200 entries with every field filled in stays far below this.
const MAX_ANSWER_BYTES: u64 = 8 << 20;
/// The most entries asked for at once. Some servers answer slowly, or not at all,
/// when asked for a whole folder of thousands.
const MAX_PAGE: u32 = 200;
/// The most entries taken from one document (a server may send more than asked for).
const MAX_ENTRIES: usize = 1000;
/// Titles, ids and the other texts of an entry are cut to this many characters.
const MAX_TEXT_CHARS: usize = 1024;
/// The most addresses kept per song: the file itself and a handful of conversions.
const MAX_RESOURCES: usize = 32;
/// What the type of every version of the ContentDirectory service begins with.
const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:";

/// A media server and where to ask it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    /// Unique device name ("uuid:…").
    pub udn: String,
    pub location: String,
    /// Where ContentDirectory takes its commands (absolute).
    pub control_url: String,
}

/// Reads the description at `location` (what a search returns, see `ssdp`).
///
/// The ContentDirectory may belong to a device inside the described one (a router
/// with a media server in it); name and UDN are then that device's. A device that
/// states no name is called by its host, one without a UDN gets its location as one,
/// so that it can still be told from the others.
pub fn describe(location: &str) -> Result<Server, NetError> {
    let base = Url::parse(location)?;
    let bytes = http::get(location, &Limits::new(MAX_DESCRIPTION_BYTES))?;
    parse_description(&String::from_utf8_lossy(&bytes), location, &base)
}

#[derive(Default)]
struct Device {
    name: Option<String>,
    udn: Option<String>,
    /// The `controlURL` of its ContentDirectory service, as written.
    control: Option<String>,
}

fn parse_description(text: &str, location: &str, base: &Url) -> Result<Server, NetError> {
    let mut reader = Reader::new(text);
    // The open elements' names, without prefix and in lower case.
    let mut path: Vec<String> = Vec::new();
    // The open `<device>` elements, outermost first.
    let mut devices: Vec<Device> = Vec::new();
    // Type and control address of the `<service>` being read.
    let mut service: Option<(String, String)> = None;
    let mut url_base: Option<String> = None;
    let mut outermost: Option<Device> = None;
    let mut chosen: Option<Device> = None;
    while let Some(event) = reader.next_event() {
        match event.map_err(|_| NetError::BadResponse("the description is not XML"))? {
            Event::Start { name, .. } => {
                let name = xml::local(&name).to_ascii_lowercase();
                match name.as_str() {
                    "device" => devices.push(Device::default()),
                    "service" => service = Some(Default::default()),
                    _ => {}
                }
                path.push(name);
            }
            Event::Text(text) => {
                let [.., parent, element] = path.as_slice() else {
                    continue;
                };
                let text = text.trim();
                let short = || capped(text, MAX_TEXT_CHARS).to_string();
                match (parent.as_str(), element.as_str()) {
                    ("device", "friendlyname") => {
                        if let Some(device) = devices.last_mut() {
                            device.name.get_or_insert_with(short);
                        }
                    }
                    ("device", "udn") => {
                        if let Some(device) = devices.last_mut() {
                            device.udn.get_or_insert_with(short);
                        }
                    }
                    ("service", "servicetype") => {
                        if let Some((kind, _)) = &mut service {
                            *kind = text.to_string();
                        }
                    }
                    ("service", "controlurl") => {
                        if let Some((_, control)) = &mut service {
                            *control = text.to_string();
                        }
                    }
                    (_, "urlbase") => {
                        url_base.get_or_insert_with(|| text.to_string());
                    }
                    _ => {}
                }
            }
            Event::End { .. } => match path.pop().as_deref() {
                Some("service") => {
                    if let Some((kind, control)) = service.take()
                        && kind.starts_with(CONTENT_DIRECTORY)
                        && !control.is_empty()
                        && let Some(device) = devices.last_mut()
                    {
                        device.control.get_or_insert(control);
                    }
                }
                Some("device") => {
                    if let Some(device) = devices.pop() {
                        if device.control.is_some() && chosen.is_none() {
                            chosen = Some(device);
                        } else if devices.is_empty() && outermost.is_none() {
                            outermost = Some(device);
                        }
                    }
                }
                _ => {}
            },
        }
    }
    let device = chosen.ok_or(NetError::BadResponse(
        "the device has no ContentDirectory service",
    ))?;
    let outermost = outermost.unwrap_or_default();
    // Addresses in a description are relative to `URLBase` when there is one (UPnP
    // 1.0), else to the description itself.
    let base = url_base
        .and_then(|url_base| Url::parse(&url_base).ok())
        .unwrap_or_else(|| base.clone());
    let control_url = base
        .join(device.control.as_deref().unwrap_or_default())
        .map_err(|_| NetError::BadResponse("the ContentDirectory's address cannot be used"))?
        .to_string();
    Ok(Server {
        name: device
            .name
            .or(outermost.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| base.host.clone()),
        udn: device
            .udn
            .or(outermost.udn)
            .filter(|udn| !udn.is_empty())
            .unwrap_or_else(|| location.to_string()),
        location: location.to_string(),
        control_url,
    })
}

/// One page of a folder's children.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// How many the folder holds in all (0 when the server does not say).
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Folder(Folder),
    Item(Item),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub id: String,
    pub title: String,
    pub child_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// Who performs the song. `None` also when only the album's artist is known.
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub art_url: Option<String>,
    /// False for pictures, videos and the like.
    pub audio: bool,
    pub resources: Vec<Resource>,
}

/// One address an item can be fetched from, and what is found there.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub url: String,
    /// The third field of protocolInfo, lower case ("audio/flac").
    pub mime: String,
    pub size: Option<u64>,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    /// The server makes this from the file on request (`DLNA.ORG_CI=1` in the fourth
    /// field of protocolInfo): another format, another bitrate. Not the file itself.
    pub converted: bool,
}

/// The children of folder `id` ("0" is the root), `count` of them from `start`.
///
/// At most 200 are asked for at once, and never 0 (which a server reads as "all of
/// them"): a folder is read page by page until `total` entries are there.
pub fn browse(server: &Server, id: &str, start: u32, count: u32) -> Result<Page, NetError> {
    let base = Url::parse(&server.control_url)?;
    let count = count.clamp(1, MAX_PAGE);
    let command = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body>\
         <u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
         <ObjectID>{}</ObjectID>\
         <BrowseFlag>BrowseDirectChildren</BrowseFlag>\
         <Filter>*</Filter>\
         <StartingIndex>{start}</StartingIndex>\
         <RequestedCount>{count}</RequestedCount>\
         <SortCriteria></SortCriteria>\
         </u:Browse>\
         </s:Body>\
         </s:Envelope>",
        xml::escape(id)
    );
    let headers = [
        ("Content-Type", "text/xml; charset=\"utf-8\""),
        (
            "SOAPACTION",
            "\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"",
        ),
    ];
    let (status, bytes) = http::post(
        &server.control_url,
        &headers,
        command.as_bytes(),
        &Limits::new(MAX_ANSWER_BYTES),
    )?;
    let answer = parse_answer(&String::from_utf8_lossy(&bytes));
    if let Some(error) = answer.error {
        return Err(NetError::Upnp(error));
    }
    if !(200..300).contains(&status) {
        return Err(NetError::Status(status));
    }
    let didl = answer
        .result
        .ok_or(NetError::BadResponse("the answer holds no Result"))?;
    Ok(Page {
        entries: parse_didl(&didl, &base),
        total: answer.total.unwrap_or(0),
    })
}

#[derive(Default)]
struct Answer {
    /// The DIDL-Lite document, unescaped.
    result: Option<String>,
    total: Option<u32>,
    /// What the server says went wrong, when the answer is a SOAP fault.
    error: Option<String>,
}

/// Takes what is needed from the answer to a `Browse`. What cannot be read is left
/// out; the caller says what is missing.
fn parse_answer(text: &str) -> Answer {
    let mut answer = Answer::default();
    let mut reader = Reader::new(text);
    let mut path: Vec<String> = Vec::new();
    let mut fault = false;
    let (mut code, mut description, mut fault_string) = (None, None, None);
    while let Some(Ok(event)) = reader.next_event() {
        match event {
            Event::Start { name, .. } => {
                let name = xml::local(&name).to_ascii_lowercase();
                match name.as_str() {
                    "fault" => fault = true,
                    // An empty folder may come as an empty Result.
                    "result" => answer.result = answer.result.or(Some(String::new())),
                    _ => {}
                }
                path.push(name);
            }
            Event::Text(text) => {
                let text = text.trim();
                let short = || capped(text, MAX_TEXT_CHARS).to_string();
                match path.last().map(String::as_str) {
                    Some("result") => {
                        if let Some(result) = &mut answer.result
                            && result.is_empty()
                        {
                            result.push_str(text);
                        }
                    }
                    Some("totalmatches") => answer.total = answer.total.or(text.parse().ok()),
                    Some("errorcode") => {
                        code.get_or_insert_with(short);
                    }
                    Some("errordescription") => {
                        description.get_or_insert_with(short);
                    }
                    Some("faultstring") => {
                        fault_string.get_or_insert_with(short);
                    }
                    _ => {}
                }
            }
            Event::End { .. } => {
                path.pop();
            }
        }
    }
    if fault || code.is_some() {
        answer.error = [description, code, fault_string]
            .into_iter()
            .flatten()
            .find(|text| !text.is_empty())
            .or_else(|| Some("the command failed".to_string()));
    }
    answer
}

/// Reads a DIDL-Lite document; addresses in it are resolved against `base`.
///
/// Entries without an id are left out (a folder without one cannot be opened), as is
/// everything after the 1000th entry and after the place where the document stops
/// being XML.
pub fn parse_didl(didl: &str, base: &Url) -> Vec<Entry> {
    let mut reader = Reader::new(didl);
    let mut entries = Vec::new();
    let mut draft: Option<Draft> = None;
    // How far inside the entry being read the reader is: 1 in one of its fields.
    let mut depth = 0usize;
    let mut field: Option<Field> = None;
    while entries.len() < MAX_ENTRIES
        && let Some(Ok(event)) = reader.next_event()
    {
        match event {
            Event::Start { name, attrs } => {
                if draft.is_none() {
                    let folder = match xml::local(&name) {
                        "container" => true,
                        "item" => false,
                        _ => continue,
                    };
                    draft = Some(Draft::new(folder, &attrs));
                    depth = 0;
                } else {
                    depth += 1;
                    if depth == 1 {
                        field = Some(Field {
                            name: xml::local(&name).to_ascii_lowercase(),
                            attrs,
                            text: String::new(),
                        });
                    }
                }
            }
            Event::Text(text) => {
                if depth == 1
                    && let Some(field) = &mut field
                {
                    field.text.push_str(&text);
                }
            }
            Event::End { .. } => {
                if depth > 0 {
                    if depth == 1
                        && let (Some(draft), Some(field)) = (&mut draft, field.take())
                    {
                        draft.take(&field, base);
                    }
                    depth -= 1;
                } else if let Some(entry) = draft.take().and_then(Draft::finish) {
                    entries.push(entry);
                }
            }
        }
    }
    entries
}

/// One element inside an entry (`<dc:title>`, `<res …>`).
struct Field {
    /// Without prefix, in lower case.
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
}

/// An entry while it is being read.
#[derive(Default)]
struct Draft {
    folder: bool,
    id: String,
    child_count: Option<u32>,
    title: Option<String>,
    creator: Option<String>,
    /// From an `<upnp:artist>` without a role…
    artist: Option<String>,
    /// …with a role other than AlbumArtist ("Performer", "Composer")…
    artist_in_role: Option<String>,
    /// …and with the role AlbumArtist.
    album_artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    track_number: Option<u32>,
    art_url: Option<String>,
    class: Option<String>,
    resources: Vec<Resource>,
}

impl Draft {
    fn new(folder: bool, attrs: &[(String, String)]) -> Draft {
        Draft {
            folder,
            id: capped(attr(attrs, "id").unwrap_or_default(), MAX_TEXT_CHARS).to_string(),
            child_count: attr(attrs, "childCount").and_then(|count| count.trim().parse().ok()),
            ..Draft::default()
        }
    }

    fn take(&mut self, field: &Field, base: &Url) {
        let text = field.text.trim();
        match field.name.as_str() {
            "title" => keep(&mut self.title, text),
            "creator" => keep(&mut self.creator, text),
            "artist" => {
                let slot = match attr(&field.attrs, "role") {
                    None => &mut self.artist,
                    Some(role) if role.trim().eq_ignore_ascii_case("AlbumArtist") => {
                        &mut self.album_artist
                    }
                    Some(_) => &mut self.artist_in_role,
                };
                keep(slot, text);
            }
            "album" => keep(&mut self.album, text),
            "genre" => keep(&mut self.genre, text),
            "class" => keep(&mut self.class, text),
            "originaltracknumber" => {
                self.track_number = self.track_number.or(text.parse().ok());
            }
            "albumarturi" => {
                if self.art_url.is_none() {
                    self.art_url = http_url(base, text);
                }
            }
            "res" => {
                if self.resources.len() < MAX_RESOURCES
                    && let Some(resource) = resource(&field.attrs, text, base)
                {
                    self.resources.push(resource);
                }
            }
            _ => {}
        }
    }

    fn finish(self) -> Option<Entry> {
        if self.id.is_empty() {
            return None;
        }
        let title = self.title.unwrap_or_else(|| self.id.clone());
        Some(if self.folder {
            Entry::Folder(Folder {
                id: self.id,
                title,
                child_count: self.child_count,
            })
        } else {
            Entry::Item(Item {
                id: self.id,
                title,
                artist: self.artist.or(self.artist_in_role).or(self.creator),
                album: self.album,
                album_artist: self.album_artist,
                genre: self.genre,
                track_number: self.track_number,
                art_url: self.art_url,
                audio: self
                    .class
                    .is_some_and(|class| class.starts_with("object.item.audioItem")),
                resources: self.resources,
            })
        })
    }
}

/// Keeps the first text that says something.
fn keep(slot: &mut Option<String>, text: &str) {
    if slot.is_none() && !text.is_empty() {
        *slot = Some(capped(text, MAX_TEXT_CHARS).to_string());
    }
}

/// The value of the attribute `name` (looked for without prefix, in any case).
fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| xml::local(key).eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The whole address `text` stands for in a document at `base`.
fn http_url(base: &Url, text: &str) -> Option<String> {
    if text.is_empty() {
        return None; // would stand for the document itself
    }
    Some(base.join(text).ok()?.to_string())
}

/// Reads a `<res>`: `None` unless it is fetched with a plain HTTP GET.
fn resource(attrs: &[(String, String)], text: &str, base: &Url) -> Option<Resource> {
    // protocolInfo is `protocol:network:format:more`.
    let mut info = attr(attrs, "protocolInfo")?.splitn(4, ':');
    if !info.next()?.trim().eq_ignore_ascii_case("http-get") {
        return None;
    }
    let _network = info.next()?;
    let mime = info.next().unwrap_or_default().trim().to_ascii_lowercase();
    let converted = info
        .next()
        .unwrap_or_default()
        .split(';')
        .any(|flag| flag.trim().eq_ignore_ascii_case("DLNA.ORG_CI=1"));
    let number = |name| attr(attrs, name).map(str::trim);
    Some(Resource {
        url: http_url(base, text)?,
        mime,
        size: number("size").and_then(|size| size.parse().ok()),
        duration_ms: number("duration").and_then(parse_duration),
        sample_rate: number("sampleFrequency").and_then(|rate| rate.parse().ok()),
        bit_depth: number("bitsPerSample").and_then(|bits| bits.parse().ok()),
        channels: number("nrAudioChannels").and_then(|channels| channels.parse().ok()),
        converted,
    })
}

/// The formats the player decodes, as far as a server's words can tell them apart.
#[derive(Clone, Copy, PartialEq)]
enum Format {
    Flac,
    Wav,
    Aiff,
    Dsf,
    Dff,
    Mp4,
    Mp3,
    Ogg,
    Opus,
    WavPack,
    Ape,
}

impl Format {
    /// MP4 holds ALAC as well as AAC and the server does not say which, so it is
    /// ranked with the lossy formats: a FLAC or WAV of the same song goes first.
    fn lossless(self) -> bool {
        matches!(
            self,
            Format::Flac
                | Format::Wav
                | Format::Aiff
                | Format::Dsf
                | Format::Dff
                | Format::WavPack
                | Format::Ape
        )
    }

    fn of_mime(mime: &str) -> Option<Format> {
        Some(match mime {
            "audio/flac" | "audio/x-flac" => Format::Flac,
            "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Format::Wav,
            "audio/aiff" | "audio/x-aiff" => Format::Aiff,
            "audio/x-dsf" | "audio/dsf" => Format::Dsf,
            "audio/x-dff" | "audio/dff" | "audio/x-dsd" | "audio/dsd" => Format::Dff,
            // Not `audio/aac`: that is AAC without the MP4 around it (ADTS), which
            // the player does not read.
            "audio/mp4" | "audio/x-m4a" | "audio/m4a" => Format::Mp4,
            "audio/mpeg" | "audio/mp3" => Format::Mp3,
            "audio/ogg" | "audio/x-ogg" | "application/ogg" => Format::Ogg,
            "audio/opus" | "audio/x-opus" => Format::Opus,
            "audio/x-wavpack" | "audio/wavpack" => Format::WavPack,
            "audio/x-ape" | "audio/ape" | "audio/x-monkeys-audio" => Format::Ape,
            _ => return None,
        })
    }

    fn of_extension(extension: &str) -> Option<Format> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "flac" => Format::Flac,
            "wav" => Format::Wav,
            "aif" | "aiff" => Format::Aiff,
            "dsf" => Format::Dsf,
            "dff" => Format::Dff,
            "m4a" | "mp4" => Format::Mp4,
            "mp3" => Format::Mp3,
            "ogg" | "oga" => Format::Ogg,
            "opus" => Format::Opus,
            "wv" => Format::WavPack,
            "ape" => Format::Ape,
            _ => return None,
        })
    }

    fn of(resource: &Resource, audio: bool) -> Option<Format> {
        if resource.converted {
            return None;
        }
        // Parameters (`;rate=44100`) are not part of the type.
        let mime = resource.mime.split(';').next().unwrap_or_default().trim();
        if let Some(format) = Format::of_mime(mime) {
            return Some(format);
        }
        // A type that says nothing: the name of the file is the next best witness.
        // Only for songs; a film's `.mp4` is not for the player. Types that do say
        // something (`audio/L16`: samples without a header) are taken at their word,
        // whatever the address ends with.
        let says_nothing = matches!(
            mime,
            "" | "*"
                | "*/*"
                | "audio/*"
                | "audio/unknown"
                | "audio/x-unknown"
                | "application/octet-stream"
                | "binary/octet-stream"
        );
        if !(audio && says_nothing) {
            return None;
        }
        let path = Url::parse(&resource.url).ok()?.path;
        let path = path.split('?').next().unwrap_or_default();
        let (_, extension) = path.rsplit_once('.')?;
        Format::of_extension(extension)
    }
}

/// The resource to play: the original file in a format the player decodes, if the
/// item offers one.
///
/// Lossless formats go before lossy ones; among equals the first listed wins (servers
/// list the file itself first). Conversions the server would make on request are
/// never chosen, nor is raw PCM (`audio/L16`), which says too little about itself.
pub fn best_resource(item: &Item) -> Option<&Resource> {
    item.resources
        .iter()
        .filter_map(|resource| Some((resource, Format::of(resource, item.audio)?)))
        // `min_by_key` returns the first of equals.
        .min_by_key(|(_, format)| !format.lossless())
        .map(|(resource, _)| resource)
}

/// "1:02:03.500" -> milliseconds.
///
/// Also read: `M:SS` and plain seconds, which some servers write. `None` for what is
/// no duration (a sign, letters, a fraction written as `1/2`).
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut fields = text.trim().rsplit(':');
    let seconds = fields.next()?;
    let minutes = fields.next();
    let hours = fields.next();
    if fields.next().is_some() {
        return None;
    }
    let whole = |digits: &str| -> Option<u64> {
        // `parse` alone would also take a sign.
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    };
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // The first three digits after the point are the milliseconds.
    let millis = (0..3).fold(0, |millis, at| {
        let digit = fraction.as_bytes().get(at).map_or(0, |digit| digit - b'0');
        millis * 10 + u64::from(digit)
    });
    let hours = hours.map_or(Some(0), whole)?;
    let minutes = minutes.map_or(Some(0), whole)?;
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(whole(seconds)?.checked_mul(1000)?)?
        .checked_add(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        for (text, expected) in [
            ("1:02:03.500", Some(3_723_500)),
            ("0:03:25", Some(205_000)),
            ("0:03:25.0", Some(205_000)),
            ("0:00:00.007", Some(7)),
            ("0:00:01.5", Some(1_500)),
            ("0:00:01.23456", Some(1_234)),
            ("0:00:01.", Some(1_000)),
            ("3:25", Some(205_000)),
            ("75", Some(75_000)),
            (" 0:00:10.000 ", Some(10_000)),
            ("100:00:00", Some(360_000_000)),
            ("", None),
            (":", None),
            ("1::3", None),
            ("0:0:0:1", None),
            ("-0:00:01", None),
            ("0:00:+1", None),
            ("0:00:01.1/2", None),
            ("0:00:.5", None),
            ("abc", None),
            ("99999999999999999999:00:00", None),
            ("18446744073709551615:00:00", None),
        ] {
            assert_eq!(parse_duration(text), expected, "{text:?}");
        }
    }

    fn base() -> Url {
        Url::parse("http://10.0.0.2:8200/ctl/ContentDir").unwrap()
    }

    fn description(body: &str) -> Result<Server, NetError> {
        let location = "http://10.0.0.2:8200/rootDesc.xml";
        parse_description(body, location, &Url::parse(location).unwrap())
    }

    #[test]
    fn a_description_names_the_device_that_has_the_content_directory() {
        let server = description(
            "<root><device><friendlyName>Router</friendlyName><UDN>uuid:router</UDN>\
             <serviceList><service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1\
             </serviceType><controlURL>/l3f</controlURL></service></serviceList>\
             <deviceList><device><friendlyName>Media</friendlyName><UDN>uuid:media</UDN>\
             <serviceList><service>\
             <serviceType>urn:schemas-upnp-org:service:ContentDirectory:2</serviceType>\
             <controlURL>cd/control</controlURL></service></serviceList></device></deviceList>\
             </device></root>",
        )
        .unwrap();
        assert_eq!(server.name, "Media");
        assert_eq!(server.udn, "uuid:media");
        assert_eq!(server.control_url, "http://10.0.0.2:8200/cd/control");
    }

    #[test]
    fn a_description_without_names_still_gives_a_server() {
        let server = description(
            "<root><device><serviceList><service>\
             <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>\
             <controlURL>/ctl</controlURL></service></serviceList></device></root>",
        )
        .unwrap();
        assert_eq!(server.name, "10.0.0.2");
        assert_eq!(server.udn, "http://10.0.0.2:8200/rootDesc.xml");
    }

    #[test]
    fn an_answer_is_a_result_or_an_error() {
        let answer = parse_answer(
            "<s:Envelope><s:Body><u:BrowseResponse><Result>&lt;DIDL-Lite/&gt;</Result>\
             <NumberReturned>0</NumberReturned><TotalMatches>12</TotalMatches>\
             </u:BrowseResponse></s:Body></s:Envelope>",
        );
        assert_eq!(answer.result.as_deref(), Some("<DIDL-Lite/>"));
        assert_eq!(answer.total, Some(12));
        assert_eq!(answer.error, None);

        let empty = parse_answer("<a><Result></Result><TotalMatches>x</TotalMatches></a>");
        assert_eq!(empty.result.as_deref(), Some(""));
        assert_eq!(empty.total, None);

        let fault = |detail: &str| {
            parse_answer(&format!(
                "<s:Envelope><s:Body><s:Fault><faultcode>s:Client</faultcode>\
                 <faultstring>UPnPError</faultstring><detail>{detail}</detail></s:Fault>\
                 </s:Body></s:Envelope>"
            ))
            .error
        };
        assert_eq!(
            fault(
                "<UPnPError><errorCode>701</errorCode>\
                 <errorDescription>No such object</errorDescription></UPnPError>"
            )
            .as_deref(),
            Some("No such object")
        );
        assert_eq!(
            fault("<UPnPError><errorCode>701</errorCode></UPnPError>").as_deref(),
            Some("701")
        );
        assert_eq!(fault("").as_deref(), Some("UPnPError"));
        assert_eq!(
            parse_answer("<Envelope><Body><Fault/></Body></Envelope>")
                .error
                .as_deref(),
            Some("the command failed")
        );
    }

    fn resource(mime: &str, url: &str) -> Resource {
        Resource {
            url: url.to_string(),
            mime: mime.to_string(),
            size: None,
            duration_ms: None,
            sample_rate: None,
            bit_depth: None,
            channels: None,
            converted: false,
        }
    }

    fn item(audio: bool, resources: Vec<Resource>) -> Item {
        Item {
            id: "1".into(),
            title: "t".into(),
            artist: None,
            album: None,
            album_artist: None,
            genre: None,
            track_number: None,
            art_url: None,
            audio,
            resources,
        }
    }

    #[test]
    fn every_decodable_type_is_known() {
        for mime in [
            "audio/flac",
            "audio/x-flac",
            "audio/wav",
            "audio/x-wav",
            "audio/wave",
            "audio/aiff",
            "audio/x-aiff",
            "audio/mp4",
            "audio/x-m4a",
            "audio/m4a",
            "audio/mpeg",
            "audio/mp3",
            "audio/ogg",
            "audio/x-ogg",
            "application/ogg",
            "audio/x-dsf",
            "audio/dsf",
            "audio/x-dff",
            "audio/dff",
            "audio/x-dsd",
            "audio/dsd",
            "audio/opus",
            "audio/x-wavpack",
            "audio/x-ape",
        ] {
            let item = item(true, vec![resource(mime, "http://h/x")]);
            assert!(best_resource(&item).is_some(), "{mime}");
        }
        for mime in [
            "audio/aac",
            "audio/l16",
            "audio/l16;rate=44100;channels=2",
            "audio/l24",
            "audio/x-ms-wma",
            "audio/x-tta",
            "video/mp4",
            "image/jpeg",
            "text/plain",
        ] {
            // Not even when the address looks like a file the player reads.
            let item = item(true, vec![resource(mime, "http://h/x.flac")]);
            assert!(best_resource(&item).is_none(), "{mime}");
        }
    }

    #[test]
    fn a_type_that_says_nothing_leaves_it_to_the_file_name() {
        let named = |mime: &str, url: &str, audio: bool| {
            best_resource(&item(audio, vec![resource(mime, url)])).is_some()
        };
        assert!(named(
            "application/octet-stream",
            "http://h/a/song.FLAC",
            true
        ));
        assert!(named("audio/*", "http://h/song.m4a?session=1.2", true));
        assert!(named("", "http://h/song.dsf", true));
        assert!(!named(
            "application/octet-stream",
            "http://h/song.wma",
            true
        ));
        assert!(!named("application/octet-stream", "http://h/song", true));
        assert!(!named(
            "application/octet-stream",
            "http://h/get?file=song.flac",
            true
        ));
        // A film is not played for having a name the player knows.
        assert!(!named(
            "application/octet-stream",
            "http://h/film.mp4",
            false
        ));
        assert!(named("audio/mp4", "http://h/film.mp4", false));
    }

    #[test]
    fn lossless_goes_first_and_then_the_order_listed() {
        let pick = |resources: Vec<Resource>| {
            best_resource(&item(true, resources)).map(|resource| resource.url.clone())
        };
        assert_eq!(
            pick(vec![
                resource("audio/mpeg", "http://h/1"),
                resource("audio/mp4", "http://h/2"),
                resource("audio/x-wav", "http://h/3"),
                resource("audio/flac", "http://h/4"),
            ])
            .as_deref(),
            Some("http://h/3")
        );
        assert_eq!(
            pick(vec![
                resource("audio/mp4", "http://h/1"),
                resource("audio/mpeg", "http://h/2"),
            ])
            .as_deref(),
            Some("http://h/1")
        );
        let mut converted = resource("audio/flac", "http://h/1");
        converted.converted = true;
        assert_eq!(
            pick(vec![
                converted.clone(),
                resource("audio/mpeg", "http://h/2")
            ])
            .as_deref(),
            Some("http://h/2")
        );
        assert_eq!(pick(vec![converted]), None);
        assert_eq!(pick(Vec::new()), None);
    }

    fn resource_from(attrs: &[(String, String)]) -> Option<Resource> {
        super::resource(attrs, "/media/1.flac", &base())
    }

    #[test]
    fn a_resource_is_read_from_its_attributes() {
        let attrs = |info: &str| {
            vec![
                ("protocolInfo".to_string(), info.to_string()),
                ("size".to_string(), "1234".to_string()),
                ("duration".to_string(), "0:01:00.250".to_string()),
                ("sampleFrequency".to_string(), "96000".to_string()),
                ("bitsPerSample".to_string(), "24".to_string()),
                ("nrAudioChannels".to_string(), "2".to_string()),
            ]
        };
        assert_eq!(
            resource_from(&attrs("http-get:*:Audio/FLAC:DLNA.ORG_OP=01;DLNA.ORG_CI=0")),
            Some(Resource {
                url: "http://10.0.0.2:8200/media/1.flac".into(),
                mime: "audio/flac".into(),
                size: Some(1234),
                duration_ms: Some(60_250),
                sample_rate: Some(96_000),
                bit_depth: Some(24),
                channels: Some(2),
                converted: false,
            })
        );
        let converted = resource_from(&attrs(
            "http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_CI=1",
        ));
        assert!(converted.is_some_and(|resource| resource.converted));
        assert_eq!(resource_from(&attrs("rtsp-rtp-udp:*:audio/flac:*")), None);
        assert_eq!(resource_from(&attrs("http-get")), None);
        assert_eq!(resource_from(&[]), None);
    }
}
