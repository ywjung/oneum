//! Media servers of the network (DLNA/UPnP) and network folders (a share of a NAS over
//! WebDAV): finding the servers, and looking through what the server or the folder
//! that is open holds. A folder is shown the way a server is: its folders, and the
//! music files in them by their names. Nothing here touches the network before the view is
//! opened or a server is asked for: the app stays offline until the listener goes
//! there.
//!
//! All talking to servers happens on threads of its own; their results come back over
//! a channel and are taken in [`Browser::poll`].
use super::icons::{self, Icon};
use super::{Action, OneumApp, theme, widgets};
use crate::app::settings::SavedServer;
use crate::library::remote::RemoteTrack;
use crate::net::folders::{self, Credentials, Folder};
use crate::net::{NetError, dav, ssdp, upnp};
use crate::platform::secrets::Secret;
use eframe::egui::{self, Align2, RichText, Sense, Ui};
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// How long the network is listened to for servers' answers.
const SEARCH_WAIT: Duration = Duration::from_secs(3);
/// How long the devices that answered are given to describe themselves, all at once.
const DESCRIBE_WAIT: Duration = Duration::from_secs(6);
/// How long one address is given to answer.
const ADDRESS_WAIT: Duration = Duration::from_secs(2);
/// A folder is read this many entries at a time…
const PAGE: u32 = 200;
/// …and no further than this (a server's "all songs" can be very long).
const MAX_ENTRIES: usize = 5000;

/// Why a network folder could not be read or added.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Why {
    /// The server does not take the name and the password.
    Refused,
    /// The server's certificate is not one that is trusted (its fingerprint).
    Untrusted(String),
    /// What was typed is no address of a folder.
    Address,
    /// The system's store does not hand out the folder's password right now.
    NoPassword,
    /// There are as many folders as are kept.
    Full,
    /// The server sends the address on to one that is not followed (to `https://`
    /// from `http://`, mostly).
    Moved,
    /// The server has nothing at that path.
    NotFound,
    /// What answers at the address is not a folder of a share (a web page, a file).
    NotAFolder,
    /// An `https://` address whose port does not speak TLS.
    NotTls,
    /// An `http://` address whose server wants the password itself, which is not
    /// sent over such a connection.
    PlainPassword,
    /// Anything else, in the words of what went wrong.
    Other(String),
}

fn why_of(error: NetError) -> Why {
    match error {
        NetError::Status(401 | 403) => Why::Refused,
        NetError::Untrusted { fingerprint } => Why::Untrusted(fingerprint),
        // (What was typed has been read as an address by now: this is one the
        // server sent the request on to.)
        NetError::BadAddress | NetError::Status(301 | 302 | 303 | 307 | 308) => Why::Moved,
        NetError::Status(404 | 410) => Why::NotFound,
        NetError::Status(200 | 405 | 501) => Why::NotAFolder,
        NetError::BadResponse(what) if what == dav::NOT_A_LISTING => Why::NotAFolder,
        NetError::BadResponse(what) if what == crate::net::tls::NOT_TLS => Why::NotTls,
        NetError::PasswordInTheOpen => Why::PlainPassword,
        other => Why::Other(other.to_string()),
    }
}

/// A network folder's listing as the view shows a server's: its folders, and of its
/// files those that are music by their names, each with its address.
fn folder_page(entries: Vec<dav::Entry>) -> upnp::Page {
    let entries: Vec<upnp::Entry> = entries
        .into_iter()
        .filter_map(|entry| {
            if entry.folder {
                return Some(upnp::Entry::Folder(upnp::Folder {
                    id: entry.url,
                    title: entry.name,
                    child_count: None,
                }));
            }
            // (A cover picture, a text file: not shown.) The kind of file is told
            // by its name: a folder gives none.
            let mime = upnp::type_of_name(&entry.name)?;
            let title = match entry.name.rsplit_once('.') {
                Some((stem, _)) if !stem.is_empty() => stem.to_string(),
                _ => entry.name.clone(),
            };
            Some(upnp::Entry::Item(upnp::Item {
                id: entry.url.clone(),
                title,
                artist: None,
                album: None,
                album_artist: None,
                genre: None,
                track_number: None,
                art_url: None,
                audio: true,
                resources: vec![upnp::Resource {
                    url: entry.url,
                    mime: mime.to_string(),
                    size: entry.size,
                    duration_ms: None,
                    sample_rate: None,
                    bit_depth: None,
                    channels: None,
                    converted: false,
                }],
            }))
        })
        .collect();
    let total = entries.len() as u32;
    upnp::Page { entries, total }
}

enum Event {
    /// A network folder was read.
    FolderPage {
        ticket: u64,
        result: Result<upnp::Page, Why>,
    },
    /// A folder the listener wants to add was asked whether it is there.
    Tried {
        folder: Folder,
        password: Secret,
        result: Result<(), Why>,
    },
    Found(Vec<upnp::Server>),
    /// A saved server was asked for by its address.
    Opened {
        ticket: u64,
        result: Result<upnp::Server, String>,
    },
    Page {
        ticket: u64,
        result: Result<upnp::Page, String>,
        /// Whether more of the folder follows.
        more: bool,
    },
    Added(Option<upnp::Server>),
}

pub(super) struct Browser {
    ctx: egui::Context,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    /// Servers that answered when the network was asked, in this run.
    found: Vec<upnp::Server>,
    searching: bool,
    /// Whether the network was asked at all (it is, once, when the view is first shown).
    searched: bool,
    /// The server that is open.
    server: Option<upnp::Server>,
    /// The folders gone into, outermost first: id and title.
    path: Vec<(String, String)>,
    entries: Vec<upnp::Entry>,
    /// The tracks among `entries` that can be played, each with its place there.
    /// Worked out when the entries change (a long folder is not gone through in
    /// every frame), and handed out by its handle.
    playable: Arc<Vec<(usize, RemoteTrack)>>,
    loading: bool,
    failed: bool,
    /// Which listing is awaited. An answer to an earlier one is dropped, and the
    /// thread that reads a long folder stops when it sees another number here.
    ticket: Arc<AtomicU64>,
    adding: bool,
    add_failed: bool,
    /// A server added by its address, for the app to keep in the settings.
    added: Option<upnp::Server>,
    /// The network folder that is open (`server` then stands for it by its name).
    folder: Option<Folder>,
    /// Why the open folder could not be read.
    pub(super) why: Option<Why>,
    /// A folder that was tried, for the app to keep or to say why not.
    tried: Option<(Folder, Secret, Result<(), Why>)>,
    /// The try under way was asked for by a verification script (there is no form).
    pub(super) scripted_try: bool,
    /// Why the last try of a folder failed (for verification scripts).
    pub(super) last_try_failed: Option<Why>,
    /// How a try of the system's store for passwords went (for verification scripts).
    pub(super) secret_selftest: Option<Result<String, String>>,
}

impl Browser {
    pub(super) fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Browser {
            ctx,
            tx,
            rx,
            found: Vec::new(),
            searching: false,
            searched: false,
            server: None,
            path: Vec::new(),
            entries: Vec::new(),
            playable: Arc::default(),
            loading: false,
            failed: false,
            ticket: Arc::new(AtomicU64::new(0)),
            adding: false,
            add_failed: false,
            added: None,
            folder: None,
            why: None,
            tried: None,
            scripted_try: false,
            last_try_failed: None,
            secret_selftest: None,
        }
    }

    fn run(&self, name: &str, job: impl FnOnce(&Sender<Event>) + Send + 'static) -> bool {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                job(&tx);
                ctx.request_repaint();
            })
            .is_ok()
    }

    /// Takes what the threads have brought. Called every frame, also with the view
    /// not shown: a server added from a dialog must reach the settings.
    pub(super) fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Found(servers) => {
                    self.searching = false;
                    self.found = servers;
                }
                Event::Opened { ticket, result } => {
                    if ticket != self.ticket.load(Ordering::Acquire) {
                        continue;
                    }
                    match result {
                        Ok(server) => self.open(server),
                        Err(error) => {
                            tracing::info!(%error, "the server did not answer");
                            self.loading = false;
                            self.failed = true;
                        }
                    }
                }
                Event::Page {
                    ticket,
                    result,
                    more,
                } => {
                    if ticket != self.ticket.load(Ordering::Acquire) {
                        continue;
                    }
                    match result {
                        Ok(page) => {
                            self.entries.extend(page.entries);
                            self.note_entries();
                            self.loading = more;
                        }
                        Err(error) => {
                            tracing::info!(%error, "could not read a folder of the server");
                            self.loading = false;
                            self.failed = true;
                        }
                    }
                }
                Event::FolderPage { ticket, result } => {
                    if ticket != self.ticket.load(Ordering::Acquire) {
                        continue;
                    }
                    self.loading = false;
                    match result {
                        Ok(page) => {
                            self.entries = page.entries;
                            self.note_entries();
                        }
                        Err(why) => {
                            // (Never the password: `Why` holds none.)
                            tracing::info!(?why, "could not read a network folder");
                            self.failed = true;
                            self.why = Some(why);
                        }
                    }
                }
                Event::Tried {
                    folder,
                    password,
                    result,
                } => self.tried = Some((folder, password, result)),
                Event::Added(server) => {
                    self.adding = false;
                    self.add_failed = server.is_none();
                    if let Some(server) = &server {
                        self.open(server.clone());
                    }
                    self.added = server;
                }
            }
        }
    }

    /// The server added by address since the last call, to be kept.
    pub(super) fn take_added(&mut self) -> Option<upnp::Server> {
        self.added.take()
    }

    /// Asks whether a folder the listener wants to add is there, as who they say.
    pub(super) fn try_folder(&mut self, folder: Folder, password: Secret) {
        self.last_try_failed = None;
        self.run("oneum-net-folder", move |tx| {
            let credentials = Credentials::of(&folder.user, &password, folder.pinned.as_deref());
            let mut folder = folder;
            let result = dav::listing(&folder.url, &credentials.access())
                .map(|listing| {
                    // The folder is kept by the address its server names it by (a
                    // server may send a request on to another of its addresses):
                    // what is in the folder lies under that one.
                    folder.url = listing.place;
                })
                .map_err(why_of);
            let _ = tx.send(Event::Tried {
                folder,
                password,
                result,
            });
        });
    }

    /// The folder that was tried since the last call, with its password and how the
    /// try went.
    pub(super) fn take_tried(&mut self) -> Option<(Folder, Secret, Result<(), Why>)> {
        self.tried.take()
    }

    /// Opens a network folder at its top.
    pub(super) fn open_network_folder(&mut self, folder: Folder) {
        self.server = Some(upnp::Server {
            name: folder.name.clone(),
            udn: format!("folder:{}", folder.id),
            location: folder.url.clone(),
            control_url: folder.url.clone(),
        });
        self.folder = Some(folder);
        self.path.clear();
        self.browse();
    }

    /// Whether what is open is a network folder (not a media server).
    pub(super) fn in_folder(&self) -> bool {
        self.folder.is_some()
    }

    /// Asks the network which media servers are there.
    pub(super) fn search(&mut self) {
        if self.searching {
            return;
        }
        self.searched = true;
        self.searching = self.run("oneum-net-search", |tx| {
            // Every device that answered is asked for its description at the same
            // time: one that is slow, or cannot be reached from here, does not hold
            // up the others, and the search as a whole ends when its time is over.
            let (done, described) = channel();
            for found in ssdp::search(SEARCH_WAIT) {
                let done = done.clone();
                let asked = std::thread::Builder::new()
                    .name("oneum-net-describe".into())
                    .spawn(move || {
                        let _ = done.send(upnp::describe_found(&found.location));
                    });
                if let Err(error) = asked {
                    tracing::debug!(%error, "a found device could not be asked");
                }
            }
            drop(done);
            let until = Instant::now() + DESCRIBE_WAIT;
            let mut servers: Vec<upnp::Server> = Vec::new();
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match described.recv_timeout(left) {
                    Ok(Ok(server)) if !servers.iter().any(|s| s.udn == server.udn) => {
                        servers.push(server);
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => tracing::debug!(%error, "not a media server after all"),
                    // All have answered, or the time is over.
                    Err(_) => break,
                }
            }
            servers.sort_by_key(|s| s.name.to_lowercase());
            let _ = tx.send(Event::Found(servers));
        });
    }

    /// Looks for a media server at what the listener typed: the address of a device
    /// description (`http://…`), or a host (name or number, with or without a port)
    /// that is then asked directly, without the network-wide question.
    pub(super) fn add_by_address(&mut self, text: &str) {
        let text = text.trim().to_owned();
        if text.is_empty() || self.adding {
            return;
        }
        self.add_failed = false;
        self.adding = self.run("oneum-net-add", move |tx| {
            let _ = tx.send(Event::Added(server_at(&text)));
        });
    }

    fn next_ticket(&mut self) -> u64 {
        self.ticket.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Opens a server that was kept: it is asked again for where its lists are (the
    /// port can have changed since).
    pub(super) fn open_saved(&mut self, saved: &SavedServer) {
        self.folder = None;
        let ticket = self.next_ticket();
        self.server = Some(upnp::Server {
            name: saved.name.clone(),
            udn: String::new(),
            location: saved.location.clone(),
            control_url: String::new(),
        });
        self.path.clear();
        self.entries.clear();
        self.note_entries();
        self.failed = false;
        let location = saved.location.clone();
        self.loading = self.run("oneum-net-open", move |tx| {
            let result = upnp::describe(&location).map_err(|e| e.to_string());
            let _ = tx.send(Event::Opened { ticket, result });
        });
    }

    pub(super) fn open(&mut self, server: upnp::Server) {
        self.folder = None;
        self.server = Some(server);
        self.path.clear();
        self.browse();
    }

    pub(super) fn open_folder(&mut self, id: &str, title: &str) {
        self.path.push((id.to_owned(), title.to_owned()));
        self.browse();
    }

    /// One folder up; out of the server from its top. False if no server was open.
    pub(super) fn up(&mut self) -> bool {
        if self.server.is_none() {
            return false;
        }
        if self.path.pop().is_some() {
            self.browse();
        } else {
            self.close();
        }
        true
    }

    pub(super) fn close(&mut self) {
        self.next_ticket();
        self.server = None;
        self.folder = None;
        self.why = None;
        self.path.clear();
        self.entries.clear();
        self.note_entries();
        self.loading = false;
        self.failed = false;
    }

    /// Reads the folder that is open, from its start.
    pub(super) fn browse(&mut self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let ticket = self.next_ticket();
        self.entries.clear();
        self.note_entries();
        self.failed = false;
        self.why = None;
        if let Some(folder) = &self.folder {
            // A network folder: one question gives all it holds. It is asked as the
            // folder's account (the address itself carries neither name nor password).
            let url = self
                .path
                .last()
                .map_or_else(|| folder.url.clone(), |(id, _)| id.clone());
            self.loading = self.run("oneum-net-folder", move |tx| {
                let credentials = folders::credentials_for(&url);
                let result = dav::list(&url, &credentials.access())
                    .map(folder_page)
                    .map_err(|error| match why_of(error) {
                        // Refused, and the name and password were not even given:
                        // the system's store did not hand the password out.
                        Why::Refused if credentials.lacks_password() => Why::NoPassword,
                        why => why,
                    });
                let _ = tx.send(Event::FolderPage { ticket, result });
            });
            return;
        }
        if server.control_url.is_empty() {
            // A kept server that has not answered yet: asked again.
            let saved = SavedServer {
                name: server.name,
                location: server.location,
            };
            self.open_saved(&saved);
            return;
        }
        let id = self
            .path
            .last()
            .map_or("0", |(id, _)| id.as_str())
            .to_owned();
        let current = Arc::clone(&self.ticket);
        self.loading = self.run("oneum-net-browse", move |tx| {
            let mut start = 0u32;
            loop {
                if current.load(Ordering::Acquire) != ticket {
                    return;
                }
                let result = upnp::browse(&server, &id, start, PAGE);
                let (got, total) = result
                    .as_ref()
                    .map_or((0, 0), |page| (page.entries.len() as u32, page.total));
                start += got;
                // A server that does not say how many there are is asked until it
                // gives less than a whole page.
                let more = result.is_ok()
                    && got > 0
                    && (start as usize) < MAX_ENTRIES
                    && if total > 0 {
                        start < total
                    } else {
                        got == PAGE
                    };
                let result = result.map_err(|e| e.to_string());
                if tx
                    .send(Event::Page {
                        ticket,
                        result,
                        more,
                    })
                    .is_err()
                    || !more
                {
                    return;
                }
            }
        });
    }

    /// Where one is on the open server: the folder's name (the server's at its top),
    /// and the way to it ("server › folder"), empty at the top. `None` with no server
    /// open.
    pub(super) fn place(&self) -> Option<(String, String)> {
        let server = self.server.as_ref()?;
        let mut names = vec![server.name.as_str()];
        names.extend(self.path.iter().map(|(_, title)| title.as_str()));
        let here = names.pop().unwrap_or_default().to_owned();
        Some((here, names.join(" › ")))
    }

    /// The tracks of the open folder that can be played, each with its place among
    /// the folder's entries.
    fn playable(&self) -> Arc<Vec<(usize, RemoteTrack)>> {
        Arc::clone(&self.playable)
    }

    /// To be called whenever `entries` has changed.
    fn note_entries(&mut self) {
        let server = self.server.as_ref().map_or("", |s| s.name.as_str());
        let playable = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                upnp::Entry::Item(item) => Some((index, remote_track(server, item)?)),
                upnp::Entry::Folder(_) => None,
            })
            .collect();
        self.playable = Arc::new(playable);
    }
}

/// The media server at an address the listener gave (see [`Browser::add_by_address`]).
fn server_at(text: &str) -> Option<upnp::Server> {
    if text
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
    {
        return upnp::describe(text).ok();
    }
    // A host: asked on the port every UPnP device listens on for such questions.
    let host = text
        .rsplit_once(':')
        .filter(|(_, port)| port.parse::<u16>().is_ok())
        .map_or(text, |(host, _)| host);
    let address = (host, 1900u16)
        .to_socket_addrs()
        .ok()?
        .find(|a| a.is_ipv4())?;
    ssdp::search_at(address, ADDRESS_WAIT)
        .iter()
        .find_map(|found| upnp::describe(&found.location).ok())
}

/// The file's kind as the app names kinds, from what the server calls it.
fn container_of(mime: &str, url: &str) -> &'static str {
    const KINDS: [&str; 11] = [
        "flac", "wav", "aiff", "m4a", "mp3", "ogg", "opus", "wv", "ape", "dsf", "dff",
    ];
    let by_mime = match mime {
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" | "audio/x-ogg" | "application/ogg" => "ogg",
        "audio/opus" | "audio/x-opus" => "opus",
        "audio/x-wavpack" | "audio/wavpack" => "wv",
        "audio/x-ape" | "audio/ape" | "audio/x-monkeys-audio" => "ape",
        "audio/x-dsf" | "audio/dsf" => "dsf",
        "audio/x-dff" | "audio/dff" | "audio/x-dsd" | "audio/dsd" => "dff",
        _ => "",
    };
    if !by_mime.is_empty() {
        return by_mime;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    let extension = match extension.as_str() {
        "aif" => "aiff",
        "mp4" => "m4a",
        "oga" => "ogg",
        other => other,
    };
    KINDS
        .iter()
        .find(|k| **k == extension)
        .copied()
        .unwrap_or("")
}

/// A server's item as a track to queue; `None` for what cannot be played (a picture, a
/// video, a format the player does not read).
fn remote_track(server: &str, item: &upnp::Item) -> Option<RemoteTrack> {
    if !item.audio {
        return None;
    }
    let resource = upnp::best_resource(item)?;
    let artist = item.artist.clone().unwrap_or_default();
    Some(RemoteTrack {
        url: resource.url.clone(),
        server: server.to_owned(),
        title: item.title.clone(),
        album_artist: item.album_artist.clone().unwrap_or_else(|| artist.clone()),
        artist,
        album: item.album.clone().unwrap_or_default(),
        track_no: item.track_number,
        genre: item.genre.clone().unwrap_or_default(),
        duration_ms: resource.duration_ms.unwrap_or(0),
        container: container_of(&resource.mime, &resource.url).to_owned(),
        sample_rate: resource.sample_rate.unwrap_or(0),
        channels: resource.channels.unwrap_or(2),
        bit_depth: resource.bit_depth,
        file_size: resource.size.unwrap_or(0),
        art_url: item.art_url.clone(),
    })
}

/// One line of the view: an icon, a title with a second line under it, and something
/// short at the right.
fn row(
    ui: &mut Ui,
    icon: Icon,
    title: &str,
    detail: &str,
    trailing: &str,
    faint: bool,
) -> egui::Response {
    let touch = theme::touch(ui.ctx());
    let height = if touch { 56.0 } else { 34.0 };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    if response.hovered() || response.has_focus() || response.is_pointer_button_down_on() {
        ui.painter()
            .rect_filled(rect, 6.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let weak = visuals.weak_text_color();
    let ink = if faint { weak } else { visuals.text_color() };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Vec2::splat(if touch { 20.0 } else { 16.0 }),
    );
    icons::paint(ui.painter(), icon_rect, icon, weak);
    let small = egui::TextStyle::Small.resolve(ui.style());
    let trailing_width = if trailing.is_empty() { 0.0 } else { 56.0 };
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 42.0, rect.top()),
        egui::pos2(rect.right() - 10.0 - trailing_width, rect.bottom()),
    );
    let clipped = ui.painter().with_clip_rect(text_rect);
    let body = egui::TextStyle::Body.resolve(ui.style());
    let title_galley = ui.painter().layout_no_wrap(title.to_owned(), body, ink);
    if detail.is_empty() {
        clipped.galley(
            egui::pos2(
                text_rect.left(),
                rect.center().y - title_galley.size().y / 2.0,
            ),
            title_galley,
            ink,
        );
    } else {
        let detail_galley = ui
            .painter()
            .layout_no_wrap(detail.to_owned(), small.clone(), weak);
        let both = title_galley.size().y + detail_galley.size().y;
        let top = rect.center().y - both / 2.0;
        let below = top + title_galley.size().y;
        clipped.galley(egui::pos2(text_rect.left(), top), title_galley, ink);
        clipped.galley(egui::pos2(text_rect.left(), below), detail_galley, weak);
    }
    if !trailing.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            Align2::RIGHT_CENTER,
            trailing,
            small,
            weak,
        );
    }
    response
}

fn length(ms: u64) -> String {
    if ms == 0 {
        String::new()
    } else {
        crate::i18n::format_time(ms)
    }
}

impl OneumApp {
    /// Every frame: what the network threads brought, and a server added by its
    /// address into the settings.
    pub(super) fn sync_network(&mut self) {
        self.network.poll();
        self.take_tried_folder();
        if let Some(server) = self.network.take_added() {
            let known = self
                .settings
                .network_servers
                .iter()
                .any(|s| s.location == server.location);
            if !known {
                // (As many as are kept: the one added longest ago makes room, or the
                // new one would be gone at the next start.)
                let kept = crate::app::settings::MAX_NETWORK_SERVERS;
                let servers = &mut self.settings.network_servers;
                if servers.len() >= kept {
                    servers.drain(..=servers.len() - kept);
                }
                self.settings.network_servers.push(SavedServer {
                    name: server.name,
                    location: server.location,
                });
            }
        }
    }

    /// The network view: the servers, or the folder that is open on one.
    pub(super) fn network_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        let touch = theme::touch(ui.ctx());
        if !self.network.searched {
            // The first time the view is shown: the one moment the app asks the
            // network by itself.
            self.network.search();
        }
        if self.network.server.is_none() {
            if !touch {
                self.view_title(ui, s.network, None);
            }
            self.network_servers(ui);
        } else {
            self.network_folder(ui, actions);
        }
    }

    fn network_servers(&mut self, ui: &mut Ui) {
        let s = self.s();
        // (Wrapped: on a phone the three buttons are wider than the screen in some
        // languages. Run off its edge, the last one could not be read, and the page
        // below was laid out as wide as the row, its text off the edge too.)
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            let touch = theme::touch(ui.ctx());
            let button = |label: &str| {
                egui::Button::new(label).min_size(egui::vec2(0.0, if touch { 40.0 } else { 0.0 }))
            };
            if ui
                .add_enabled(!self.network.searching, button(s.network_search))
                .clicked()
            {
                self.network.search();
            }
            if ui.add(button(s.network_add)).clicked() {
                self.dialog = Some(super::dialogs::Dialog::AddServer {
                    name: String::new(),
                    focus: true,
                });
            }
            if ui.add(button(s.folder_add)).clicked() {
                self.folder_form = Some(super::network_folders::FolderForm::new());
            }
            if self.network.searching || self.network.adding {
                ui.spinner();
                ui.label(RichText::new(s.network_searching).color(theme::dim(ui)));
            }
        });
        if self.network.add_failed {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(s.network_add_failed).color(ui.visuals().warn_fg_color));
            });
        }
        ui.add_space(8.0);
        let saved = self.settings.network_servers.clone();
        let found: Vec<upnp::Server> = self
            .network
            .found
            .iter()
            .filter(|server| !saved.iter().any(|s| s.location == server.location))
            .cloned()
            .collect();
        let shares = self.settings.network_folders.clone();
        let mut open_share = None;
        let mut forget_share = None;
        let mut open_saved = None;
        let mut forget = None;
        let mut open_found = None;
        egui::ScrollArea::vertical()
            .id_salt("network-servers")
            .auto_shrink(false)
            .show(ui, |ui| {
                if !shares.is_empty() {
                    section(ui, s.folder_list);
                    for (index, share) in shares.iter().enumerate() {
                        let response = row(ui, Icon::Folder, &share.name, &share.url, "", false);
                        if response.clicked() {
                            open_share = Some(index);
                        }
                        response.context_menu(|ui| {
                            if ui.button(s.network_forget).clicked() {
                                forget_share = Some(index);
                                ui.close();
                            }
                        });
                    }
                    ui.add_space(8.0);
                }
                if !saved.is_empty() {
                    section(ui, s.network_saved);
                    for (index, server) in saved.iter().enumerate() {
                        let response =
                            row(ui, Icon::Network, &server.name, &server.location, "", false);
                        if response.clicked() {
                            open_saved = Some(index);
                        }
                        response.context_menu(|ui| {
                            if ui.button(s.network_forget).clicked() {
                                forget = Some(index);
                                ui.close();
                            }
                        });
                    }
                    ui.add_space(8.0);
                }
                if !found.is_empty() {
                    section(ui, s.network_found);
                    for server in &found {
                        if row(ui, Icon::Network, &server.name, "", "", false).clicked() {
                            open_found = Some(server.clone());
                        }
                    }
                }
                if saved.is_empty()
                    && found.is_empty()
                    && shares.is_empty()
                    && !self.network.searching
                {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(s.network_none).color(theme::dim(ui)));
                    });
                }
            });
        if let Some(index) = forget_share {
            self.forget_network_folder(index);
        } else if let Some(index) = open_share {
            self.network.open_network_folder(shares[index].clone());
        } else if let Some(index) = forget {
            self.settings.network_servers.remove(index);
        } else if let Some(index) = open_saved {
            self.network.open_saved(&saved[index]);
        } else if let Some(server) = open_found {
            self.network.open(server);
        }
    }

    fn network_folder(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        let touch = theme::touch(ui.ctx());
        let playable = self.network.playable();
        let (mut up, mut reload, mut play_all, mut queue_all) = (false, false, false, false);
        let (here, above) = self.network.place().unwrap_or_default();
        if touch {
            // The page's own bar has the way back and the folder's name (see
            // `phone_nav_title`); here only where the folder is.
            if !above.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(RichText::new(above).small().color(theme::dim(ui)))
                            .truncate(),
                    );
                });
            }
        } else {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                up = widgets::icon_button(ui, Icon::Back, s.back, 28.0, false).clicked();
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(here).strong()).truncate());
                    if !above.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(above).small().color(theme::dim(ui)))
                                .truncate(),
                        );
                    }
                });
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let any = !playable.is_empty();
            let button = |label: &str| {
                egui::Button::new(label).min_size(egui::vec2(0.0, if touch { 40.0 } else { 0.0 }))
            };
            play_all = ui.add_enabled(any, button(s.play)).clicked();
            queue_all = ui.add_enabled(any, button(s.add_to_queue)).clicked();
            reload = ui.add(button(s.network_reload)).clicked();
            if self.network.loading {
                ui.spinner();
            }
        });
        ui.add_space(6.0);
        let mut open: Option<(String, String)> = None;
        // What to do with the track at this place among the playable ones.
        let mut act: Option<(usize, u8)> = None;
        let entries = &self.network.entries;
        egui::ScrollArea::vertical()
            .id_salt(("network-folder", self.network.path.len()))
            .auto_shrink(false)
            .show(ui, |ui| {
                if self.network.failed {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        // A network folder says why.
                        let pinned = self
                            .network
                            .folder
                            .as_ref()
                            .is_some_and(|folder| folder.pinned.is_some());
                        let words = match &self.network.why {
                            Some(Why::Refused) => s.folder_refused_later,
                            Some(Why::Untrusted(_)) if pinned => s.folder_changed_certificate,
                            Some(Why::Untrusted(_)) => s.folder_untrusted_later,
                            Some(Why::NoPassword) => s.folder_no_password,
                            Some(Why::NotFound) => s.folder_not_found,
                            Some(Why::PlainPassword) => s.folder_plain_password_later,
                            _ => s.network_failed,
                        };
                        ui.label(RichText::new(words).color(theme::dim(ui)));
                    });
                    return;
                }
                if entries.is_empty() && !self.network.loading {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(s.network_empty).color(theme::dim(ui)));
                    });
                    return;
                }
                let mut next_playable = 0usize;
                for (index, entry) in entries.iter().enumerate() {
                    match entry {
                        upnp::Entry::Folder(folder) => {
                            let count = folder
                                .child_count
                                .map(|n| n.to_string())
                                .unwrap_or_default();
                            if row(ui, Icon::Folder, &folder.title, "", &count, false).clicked() {
                                open = Some((folder.id.clone(), folder.title.clone()));
                            }
                        }
                        upnp::Entry::Item(item) => {
                            let place = playable
                                .get(next_playable)
                                .filter(|(at, _)| *at == index)
                                .map(|_| next_playable);
                            if place.is_some() {
                                next_playable += 1;
                            }
                            let detail = match (&item.artist, &item.album) {
                                (Some(artist), Some(album)) => format!("{artist} — {album}"),
                                (Some(one), None) | (None, Some(one)) => one.clone(),
                                (None, None) => String::new(),
                            };
                            let time = place
                                .map(|p| length(playable[p].1.duration_ms))
                                .unwrap_or_default();
                            let response =
                                row(ui, Icon::Note, &item.title, &detail, &time, place.is_none());
                            let Some(place) = place else {
                                continue;
                            };
                            // A finger plays with a tap; a pointer with two clicks, as
                            // in the song lists.
                            if (touch && response.clicked()) || response.double_clicked() {
                                act = Some((place, 0));
                            }
                            response.context_menu(|ui| {
                                for (what, label) in
                                    [(0u8, s.play), (1, s.play_next), (2, s.add_to_queue)]
                                {
                                    if ui.button(label).clicked() {
                                        act = Some((place, what));
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }
                }
            });
        if up {
            self.network.up();
        } else if reload {
            self.network.browse();
        } else if let Some((id, title)) = open {
            self.network.open_folder(&id, &title);
        } else if play_all || queue_all || act.is_some() {
            let tracks: Vec<RemoteTrack> = match act {
                // Playing one plays the folder from there; the other two take the one.
                Some((place, what)) if what != 0 => vec![playable[place].1.clone()],
                _ => playable.iter().map(|(_, track)| track.clone()).collect(),
            };
            let ids = self.core.library.add_remote(&tracks);
            if ids.is_empty() {
                return;
            }
            actions.push(match act {
                Some((place, 0)) => Action::PlayTracks(ids, place),
                Some((_, 1)) => Action::PlayNext(ids),
                Some(_) => Action::Enqueue(ids),
                None if play_all => Action::PlayTracks(ids, 0),
                None => Action::Enqueue(ids),
            });
        }
    }

    /// The network view by words, for verification scripts (see `automation`).
    pub(super) fn network_script(&mut self, words: &str) {
        let mut parts = words.split_whitespace();
        let (verb, argument) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let number = argument.parse::<usize>().ok();
        match (verb, number) {
            ("add", _) => self.network.add_by_address(argument),
            ("up", _) => {
                self.network.up();
            }
            ("reload", _) => self.network.browse(),
            ("saved", Some(n)) => {
                if let Some(saved) = self.settings.network_servers.get(n).cloned() {
                    self.network.open_saved(&saved);
                }
            }
            ("forget", Some(n)) if n < self.settings.network_servers.len() => {
                self.settings.network_servers.remove(n);
            }
            // A network folder: `folder-add <address> [name] [password]` tries it and
            // keeps it when it answers; `folder-trust` goes on with the certificate
            // the last try was refused for; `folder-open <n>`, `folder-forget <n>`.
            ("folder-add", _) | ("folder-trust", _) => {
                let (user, password) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let pinned = match (&self.network.last_try_failed, verb) {
                    (Some(Why::Untrusted(fingerprint)), "folder-trust") => {
                        Some(fingerprint.clone())
                    }
                    _ => None,
                };
                let folder = Folder {
                    id: folders::new_id(),
                    name: String::new(),
                    url: super::network_folders::folder_address(argument),
                    user: user.to_string(),
                    pinned,
                }
                .sanitized();
                match folder {
                    Some(folder) => {
                        self.network.scripted_try = true;
                        self.network.try_folder(folder, Secret::new(password));
                    }
                    None => self.network.last_try_failed = Some(Why::Address),
                }
            }
            ("folder-open", Some(n)) => {
                if let Some(folder) = self.settings.network_folders.get(n).cloned() {
                    self.network.open_network_folder(folder);
                }
            }
            ("folder-forget", Some(n)) => self.forget_network_folder(n),
            // The system's own store for passwords, tried with a password made for
            // the purpose (a verification run keeps the folders' in memory).
            ("secret-selftest", _) => {
                self.network.secret_selftest =
                    Some(crate::platform::secrets::try_system_store().map(str::to_string));
            }
            // What a tap on that entry does: into a folder, or play the folder from
            // that track.
            ("open", Some(n)) => match self.network.entries.get(n) {
                Some(upnp::Entry::Folder(folder)) => {
                    let (id, title) = (folder.id.clone(), folder.title.clone());
                    self.network.open_folder(&id, &title);
                }
                Some(upnp::Entry::Item(_)) => {
                    let playable = self.network.playable();
                    let Some(place) = playable.iter().position(|(at, _)| *at == n) else {
                        return;
                    };
                    let tracks: Vec<RemoteTrack> =
                        playable.iter().map(|(_, track)| track.clone()).collect();
                    let ids = self.core.library.add_remote(&tracks);
                    if !ids.is_empty() {
                        self.apply(Action::PlayTracks(ids, place));
                    }
                }
                None => {}
            },
            _ => tracing::warn!(words, "net: not understood"),
        }
    }

    /// What the network view holds, for verification scripts.
    pub(super) fn network_state(&self) -> serde_json::Value {
        let network = &self.network;
        let title = |entry: &upnp::Entry| match entry {
            upnp::Entry::Folder(folder) => folder.title.clone(),
            upnp::Entry::Item(item) => item.title.clone(),
        };
        serde_json::json!({
            "server": network.server.as_ref().map(|s| s.name.clone()),
            "depth": network.path.len(),
            "entries": network.entries.len(),
            "folders": network
                .entries
                .iter()
                .filter(|e| matches!(e, upnp::Entry::Folder(_)))
                .count(),
            "playable": network.playable().len(),
            "titles": network.entries.iter().take(8).map(title).collect::<Vec<_>>(),
            "loading": network.loading,
            "failed": network.failed,
            "searching": network.searching,
            "found": network.found.len(),
            "adding": network.adding,
            "add_failed": network.add_failed,
            "saved": self
                .settings
                .network_servers
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
            // (Names, addresses and who to be there; a password is in neither the
            // settings nor here.)
            "shares": self
                .settings
                .network_folders
                .iter()
                .map(|f| serde_json::json!({
                    "name": f.name,
                    "url": f.url,
                    "user": f.user,
                    "pinned": f.pinned,
                }))
                .collect::<Vec<_>>(),
            "in_share": network.in_folder(),
            "share_failed": network.why.as_ref().or(network.last_try_failed.as_ref()).map(|why| match why {
                Why::Refused => "refused".to_string(),
                Why::Untrusted(_) => "untrusted".to_string(),
                Why::Address => "address".to_string(),
                Why::NoPassword => "no password".to_string(),
                Why::Full => "full".to_string(),
                Why::Moved => "moved".to_string(),
                Why::NotFound => "not found".to_string(),
                Why::NotAFolder => "not a folder".to_string(),
                Why::NotTls => "not tls".to_string(),
                Why::PlainPassword => "plain password".to_string(),
                Why::Other(reason) => format!("other: {reason}"),
            }),
            "secret_selftest": network.secret_selftest.clone().map(|r| match r {
                Ok(done) => format!("ok: {done}"),
                Err(why) => format!("failed: {why}"),
            }),
        })
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(title).small().strong().color(theme::dim(ui)));
    });
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(url: &str, mime: &str) -> upnp::Resource {
        upnp::Resource {
            url: url.to_owned(),
            mime: mime.to_owned(),
            size: Some(1000),
            duration_ms: Some(61_000),
            sample_rate: Some(96_000),
            bit_depth: Some(24),
            channels: Some(2),
            converted: false,
        }
    }

    #[test]
    fn the_kind_of_file_is_told_by_what_the_server_calls_it_and_else_by_its_name() {
        assert_eq!(container_of("audio/x-flac", "http://h/1"), "flac");
        assert_eq!(container_of("audio/mpeg", "http://h/1.flac"), "mp3");
        assert_eq!(container_of("audio/x-dsd", "http://h/1"), "dff");
        // A server that calls everything by one name: the name in the address tells.
        assert_eq!(
            container_of("application/octet-stream", "http://h/a/b.M4A?x=1"),
            "m4a"
        );
        assert_eq!(container_of("audio/unknown", "http://h/a/b.aif"), "aiff");
        assert_eq!(container_of("", "http://h/stream?id=3"), "");
        assert_eq!(container_of("", "http://h/a.exe"), "");
    }

    #[test]
    fn only_audio_becomes_a_track() {
        let mut item = upnp::Item {
            id: "7".into(),
            title: "곡".into(),
            artist: Some("누구".into()),
            album: None,
            album_artist: None,
            genre: None,
            track_number: Some(2),
            art_url: None,
            audio: false,
            resources: vec![resource("http://h/7.flac", "audio/flac")],
        };
        assert!(
            remote_track("서버", &item).is_none(),
            "a picture or a video"
        );
        item.audio = true;
        let track = remote_track("서버", &item).expect("a song the player reads");
        assert_eq!(track.url, "http://h/7.flac");
        assert_eq!(
            (track.container.as_str(), track.duration_ms),
            ("flac", 61_000)
        );
        assert_eq!((track.sample_rate, track.bit_depth), (96_000, Some(24)));
        assert_eq!(
            track.album_artist, "누구",
            "the artist stands in for the album's"
        );
        // What the server only gives converted is not offered.
        item.resources[0].converted = true;
        assert!(remote_track("서버", &item).is_none());
        item.resources.clear();
        assert!(remote_track("서버", &item).is_none(), "nothing to fetch");
    }

    #[test]
    fn a_length_the_server_does_not_tell_is_left_empty() {
        assert_eq!(length(0), "");
        assert_eq!(length(61_000), "1:01");
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    /// A network folder's files are shown like a server's songs: the music among
    /// them, by its name, whatever the address begins with.
    #[test]
    fn a_folders_music_files_are_songs_and_the_rest_is_left_out() {
        let entry = |name: &str, folder: bool| dav::Entry {
            name: name.to_string(),
            url: format!("https://nas.local:5006/music/{}", name.replace(' ', "%20")),
            folder,
            size: (!folder).then_some(1_000),
        };
        let page = folder_page(vec![
            entry("Albums", true),
            entry("01 First.flac", false),
            entry("02 Second.WV", false),
            entry("cover.jpg", false),
            entry("notes.txt", false),
            entry("book.m4b", false),
            entry("old.mp2", false),
            entry("noextension", false),
        ]);
        let titles: Vec<String> = page
            .entries
            .iter()
            .map(|entry| match entry {
                upnp::Entry::Folder(folder) => format!("[{}]", folder.title),
                upnp::Entry::Item(item) => item.title.clone(),
            })
            .collect();
        assert_eq!(titles, ["[Albums]", "01 First", "02 Second", "book", "old"]);
        // Each can be queued, with its address as it is and the kind its name says.
        let tracks: Vec<RemoteTrack> = page
            .entries
            .iter()
            .filter_map(|entry| match entry {
                upnp::Entry::Item(item) => remote_track("NAS", item),
                upnp::Entry::Folder(_) => None,
            })
            .collect();
        assert_eq!(tracks.len(), 4);
        assert_eq!(
            tracks[0].url,
            "https://nas.local:5006/music/01%20First.flac"
        );
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.container.as_str())
                .collect::<Vec<_>>(),
            ["flac", "wv", "m4a", "mp3"]
        );
        assert_eq!(tracks[0].server, "NAS");
    }

    #[test]
    fn a_failure_says_what_the_listener_can_do_about_it() {
        assert_eq!(why_of(NetError::Status(401)), Why::Refused);
        assert_eq!(why_of(NetError::Status(403)), Why::Refused);
        assert_eq!(
            why_of(NetError::Untrusted {
                fingerprint: "ab".into()
            }),
            Why::Untrusted("ab".into())
        );
        // An address the server sends on to and that is not followed, a path it
        // does not have, a page that is no folder, a port that does not speak TLS.
        assert_eq!(why_of(NetError::BadAddress), Why::Moved);
        assert_eq!(why_of(NetError::Status(301)), Why::Moved);
        assert_eq!(why_of(NetError::Status(404)), Why::NotFound);
        assert_eq!(why_of(NetError::Status(200)), Why::NotAFolder);
        assert_eq!(why_of(NetError::Status(405)), Why::NotAFolder);
        assert_eq!(
            why_of(NetError::BadResponse(dav::NOT_A_LISTING)),
            Why::NotAFolder
        );
        assert_eq!(
            why_of(NetError::BadResponse(crate::net::tls::NOT_TLS)),
            Why::NotTls
        );
        assert!(matches!(why_of(NetError::Status(500)), Why::Other(_)));
        assert!(matches!(why_of(NetError::Timeout), Why::Other(_)));
    }
}
