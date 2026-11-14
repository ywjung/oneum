//! Scripted UI driving for verification (inactive unless `ONEUM_SCRIPT` is set).
//!
//! The app reads commands appended to the script file and feeds them into egui as real
//! input events (pointer, keys, text), so hit-testing, focus and shortcuts are exercised
//! exactly as with a mouse and keyboard. `shot` and `dump` report what happened.
//!
//! Commands (one per line, coordinates in points):
//!   click X Y [cmd] [shift] · dclick X Y · rclick X Y · move X Y · drag X1 Y1 X2 Y2
//!   scroll X Y DY · key NAME [cmd] [shift] [alt] · text STRING · wait MS
//!   tap X Y · hold X Y [MS] · swipe X1 Y1 X2 Y2 [STEPS] (a finger on a touch screen:
//!   touch events with the pointer events a touch screen sends along; a swipe moves
//!   one step a frame, so it carries the speed that lets a list coast on)
//!   finger down|move|up X Y (the parts of a gesture one by one, to look at the screen
//!   while a finger is still on it)
//!   shot FILE.png · dump FILE.json · open PATH · volume V · quit
//!   oskey KEYCODE [cmd] [shift] [alt] (real OS key event; the app has to be the active one)
//!   menukey KEYCODE [cmd] [shift] [alt] (the same key handed to the menu bar; works in the background)
//!   · inputsource ID · activate · hide
//!   front / back (keep the window above others without taking focus, and undo) · resize W H · menu ID (a menu-bar command: song_play_next, toggle_mute, …)
//!   (`menu` also takes loudness_start / loudness_stop / loudness_forget: the buttons of the
//!   analysis card in Settings)
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, pos2};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Steps the app itself has to carry out.
pub(super) enum AppStep {
    Shot(PathBuf),
    Dump(PathBuf),
    Open(PathBuf),
    Volume(f32),
    Quit,
    OsKey(u16, bool, bool, bool),
    /// A key press handed straight to the menu bar's key equivalents.
    MenuKey(u16, bool, bool, bool),
    InputSource(String),
    Activate,
    Hide,
    Resize(f32, f32),
    /// A menu-bar command by its id (stands in for choosing the menu item).
    Menu(String),
    /// Keep the window in front without taking keyboard focus, until `Back`.
    Front,
    Back,
}

enum Step {
    Input(Vec<Event>, Modifiers),
    /// Like `Input`, but consecutive gesture steps are timestamped 20 ms apart no matter
    /// how slowly frames arrive (the OS throttles frames of background windows, which
    /// would otherwise turn a double-click into two single clicks).
    Gesture(Vec<Event>, Modifiers),
    Wait(u64),
    App(AppStep),
}

pub(super) struct Automation {
    path: PathBuf,
    offset: u64,
    partial: String,
    steps: VecDeque<Step>,
    wait_until: Option<Instant>,
    gesture_time: Option<f64>,
    pub(super) app_steps: Vec<AppStep>,
}

fn mods(words: &[&str]) -> Modifiers {
    let mut m = Modifiers::default();
    for w in words {
        match *w {
            "cmd" => {
                m.command = true;
                m.mac_cmd = true;
            }
            "ctrl" => m.ctrl = true,
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            _ => {}
        }
    }
    m
}

fn button(pos: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

/// What a touch screen sends for one finger at `pos`: the touch itself and the pointer
/// event that stands for it (as `egui-winit` does for a real touch).
fn touch(pos: Pos2, phase: egui::TouchPhase) -> Vec<Event> {
    let none = Modifiers::default();
    let finger = Event::Touch {
        device_id: egui::TouchDeviceId(1),
        id: egui::TouchId(1),
        phase,
        pos,
        force: None,
    };
    match phase {
        egui::TouchPhase::Start => vec![
            finger,
            Event::PointerMoved(pos),
            button(pos, PointerButton::Primary, true, none),
        ],
        egui::TouchPhase::Move => vec![finger, Event::PointerMoved(pos)],
        egui::TouchPhase::End => vec![
            finger,
            button(pos, PointerButton::Primary, false, none),
            Event::PointerGone,
        ],
        egui::TouchPhase::Cancel => vec![finger, Event::PointerGone],
    }
}

/// The verification channel exists only in builds made for verification: debug builds
/// and release builds with the `automation` feature (what `scripts/dev/run.sh` builds).
/// A script can do everything the user can and write files where it likes, so the app
/// that is given to users must not listen for one, whatever its environment says.
pub(super) const AVAILABLE: bool = cfg!(any(debug_assertions, feature = "automation"));

impl Automation {
    pub(super) fn from_env() -> Option<Self> {
        if !AVAILABLE {
            return None;
        }
        let path = PathBuf::from(std::env::var_os("ONEUM_SCRIPT")?);
        Some(Self {
            path,
            offset: 0,
            partial: String::new(),
            steps: VecDeque::new(),
            wait_until: None,
            gesture_time: None,
            app_steps: Vec::new(),
        })
    }

    /// How soon the next frame is needed to keep the script moving.
    pub(super) fn poll_interval(&self) -> Duration {
        Duration::from_millis(if self.steps.is_empty() && self.wait_until.is_none() {
            100
        } else {
            16
        })
    }

    fn read_new_lines(&mut self) {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut text = String::new();
        if let Ok(n) = file.read_to_string(&mut text) {
            self.offset += n as u64;
        }
        self.partial.push_str(&text);
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            self.parse(line.trim());
        }
    }

    fn parse(&mut self, line: &str) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| words.get(i).and_then(|w| w.parse::<f32>().ok());
        let point = |i: usize| Some(pos2(num(i)?, num(i + 1)?));
        let rest = |n: usize| {
            line.splitn(n + 1, char::is_whitespace)
                .nth(n)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let none = Modifiers::default();
        match words.first().copied() {
            Some("click" | "dclick" | "rclick") => {
                let Some(p) = point(1) else { return };
                let m = mods(&words[3..]);
                let which = if words[0] == "rclick" {
                    PointerButton::Secondary
                } else {
                    PointerButton::Primary
                };
                self.steps
                    .push_back(Step::Input(vec![Event::PointerMoved(p)], m));
                let times = if words[0] == "dclick" { 2 } else { 1 };
                for _ in 0..times {
                    self.steps
                        .push_back(Step::Gesture(vec![button(p, which, true, m)], m));
                    self.steps
                        .push_back(Step::Gesture(vec![button(p, which, false, m)], m));
                }
            }
            Some("move") => {
                if let Some(p) = point(1) {
                    self.steps
                        .push_back(Step::Input(vec![Event::PointerMoved(p)], none));
                }
            }
            Some("drag") => {
                let (Some(a), Some(b)) = (point(1), point(3)) else {
                    return;
                };
                self.steps
                    .push_back(Step::Input(vec![Event::PointerMoved(a)], none));
                self.steps.push_back(Step::Input(
                    vec![button(a, PointerButton::Primary, true, none)],
                    none,
                ));
                for i in 1..=8 {
                    let t = i as f32 / 8.0;
                    self.steps.push_back(Step::Input(
                        vec![Event::PointerMoved(a + (b - a) * t)],
                        none,
                    ));
                }
                self.steps.push_back(Step::Input(
                    vec![button(b, PointerButton::Primary, false, none)],
                    none,
                ));
            }
            Some("tap" | "hold") => {
                let Some(p) = point(1) else { return };
                self.steps
                    .push_back(Step::Input(touch(p, egui::TouchPhase::Start), none));
                if words[0] == "hold" {
                    // Long enough for a long press unless told otherwise.
                    self.steps
                        .push_back(Step::Wait(num(3).map_or(1000, |ms| ms.max(0.0) as u64)));
                }
                self.steps
                    .push_back(Step::Gesture(touch(p, egui::TouchPhase::End), none));
            }
            Some("finger") => {
                let (Some(phase), Some(p)) = (words.get(1), point(2)) else {
                    return;
                };
                let phase = match *phase {
                    "down" => egui::TouchPhase::Start,
                    "move" => egui::TouchPhase::Move,
                    "up" => egui::TouchPhase::End,
                    _ => return,
                };
                self.steps.push_back(Step::Input(touch(p, phase), none));
            }
            Some("swipe") => {
                let (Some(a), Some(b)) = (point(1), point(3)) else {
                    return;
                };
                let count = num(5).map_or(12, |n| (n as usize).clamp(1, 240));
                self.steps
                    .push_back(Step::Input(touch(a, egui::TouchPhase::Start), none));
                for i in 1..=count {
                    let t = i as f32 / count as f32;
                    self.steps.push_back(Step::Input(
                        touch(a + (b - a) * t, egui::TouchPhase::Move),
                        none,
                    ));
                }
                self.steps
                    .push_back(Step::Input(touch(b, egui::TouchPhase::End), none));
            }
            Some("scroll") => {
                let (Some(p), Some(dy)) = (point(1), num(3)) else {
                    return;
                };
                self.steps
                    .push_back(Step::Input(vec![Event::PointerMoved(p)], none));
                self.steps.push_back(Step::Input(
                    vec![Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, dy),
                        phase: egui::TouchPhase::Move,
                        modifiers: none,
                        source: egui::MouseWheelSource::Wheel,
                    }],
                    none,
                ));
            }
            Some("key") => {
                let Some(key) = words.get(1).and_then(|n| Key::from_name(n)) else {
                    return;
                };
                let m = mods(&words[2..]);
                let event = |pressed| Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: m,
                };
                self.steps.push_back(Step::Input(vec![event(true)], m));
                self.steps.push_back(Step::Input(vec![event(false)], m));
            }
            Some("text") => self
                .steps
                .push_back(Step::Input(vec![Event::Text(rest(1))], none)),
            Some("wait") => self
                .steps
                .push_back(Step::Wait(num(1).unwrap_or(100.0) as u64)),
            Some("shot") => self
                .steps
                .push_back(Step::App(AppStep::Shot(PathBuf::from(rest(1))))),
            Some("dump") => self
                .steps
                .push_back(Step::App(AppStep::Dump(PathBuf::from(rest(1))))),
            Some("open") => self
                .steps
                .push_back(Step::App(AppStep::Open(PathBuf::from(rest(1))))),
            Some("volume") => self
                .steps
                .push_back(Step::App(AppStep::Volume(num(1).unwrap_or(0.0)))),
            Some("quit") => self.steps.push_back(Step::App(AppStep::Quit)),
            Some("oskey") => {
                let Some(code) = words.get(1).and_then(|w| w.parse::<u16>().ok()) else {
                    return;
                };
                let (cmd, shift) = (words.contains(&"cmd"), words.contains(&"shift"));
                let alt = words.contains(&"alt");
                self.steps
                    .push_back(Step::App(AppStep::OsKey(code, cmd, shift, alt)));
            }
            Some("menukey") => {
                let Some(code) = words.get(1).and_then(|w| w.parse::<u16>().ok()) else {
                    return;
                };
                self.steps.push_back(Step::App(AppStep::MenuKey(
                    code,
                    words.contains(&"cmd"),
                    words.contains(&"shift"),
                    words.contains(&"alt"),
                )));
            }
            Some("inputsource") => self
                .steps
                .push_back(Step::App(AppStep::InputSource(rest(1)))),
            Some("resize") => {
                if let (Some(w), Some(h)) = (num(1), num(2)) {
                    self.steps.push_back(Step::App(AppStep::Resize(w, h)));
                }
            }
            Some("menu") => self.steps.push_back(Step::App(AppStep::Menu(rest(1)))),
            Some("activate") => self.steps.push_back(Step::App(AppStep::Activate)),
            Some("front") => self.steps.push_back(Step::App(AppStep::Front)),
            Some("back") => self.steps.push_back(Step::App(AppStep::Back)),
            Some("hide") => self.steps.push_back(Step::App(AppStep::Hide)),
            _ => {}
        }
    }

    /// Called from `raw_input_hook`: performs at most one step per frame.
    pub(super) fn step(&mut self, raw: &mut egui::RawInput) {
        self.read_new_lines();
        if let Some(until) = self.wait_until {
            if Instant::now() < until {
                return;
            }
            self.wait_until = None;
        }
        match self.steps.pop_front() {
            Some(Step::Input(events, modifiers)) => {
                self.gesture_time = None;
                raw.events.push(Event::ModifiersChanged(modifiers));
                raw.events.extend(events);
            }
            Some(Step::Gesture(events, modifiers)) => {
                // Never ahead of the real clock (egui requires monotonic time), but at
                // most 20 ms after the previous gesture step when frames arrive slowly.
                let time = match (self.gesture_time, raw.time) {
                    (Some(previous), Some(now)) if now - previous > 0.05 => Some(previous + 0.02),
                    (_, now) => now,
                };
                self.gesture_time = time;
                raw.time = time;
                raw.events.push(Event::ModifiersChanged(modifiers));
                raw.events.extend(events);
            }
            Some(Step::Wait(ms)) => {
                self.gesture_time = None;
                self.wait_until = Some(Instant::now() + Duration::from_millis(ms));
            }
            Some(Step::App(step)) => {
                self.gesture_time = None;
                self.app_steps.push(step);
            }
            None => self.gesture_time = None,
        }
    }
}
