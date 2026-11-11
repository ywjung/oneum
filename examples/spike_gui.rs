//! Tech spike: eframe window + native macOS menu + IME text field + 50k-row
//! virtualized table + Now Playing / remote commands. Throwaway verification code.
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use std::sync::{Arc, Mutex};

fn log(buf: &Arc<Mutex<Vec<String>>>, s: String) {
    println!("[spike] {s}");
    if let Ok(mut b) = buf.lock() {
        b.push(s);
    }
}

mod ime {
    //! Self-driving IME check: posts real key events to our own window while the
    //! Korean 2-Set input source is selected, so composition goes through the
    //! system input method exactly as with a physical keyboard.
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSProcessInfo, NSString};
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: *const c_void;
        fn TISCreateInputSourceList(props: *const c_void, include_all: u8) -> *const c_void;
        fn TISSelectInputSource(src: *const c_void) -> i32;
    }

    pub fn select(id: &str) -> bool {
        let _ = (unsafe { kTISPropertyInputSourceID }, TISCreateInputSourceList as usize, TISSelectInputSource as usize);
        let _ = std::any::type_name::<(Retained<AnyObject>, NSArray<AnyObject>, NSDictionary<NSString, NSString>)>();
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let Some(ctx) = objc2_app_kit::NSTextInputContext::currentInputContext(mtm) else { return false };
        ctx.setSelectedKeyboardInputSource(Some(&NSString::from_str(id)));
        true
    }

    pub fn current() -> String {
        let Some(mtm) = MainThreadMarker::new() else { return "no-mtm".into() };
        match objc2_app_kit::NSTextInputContext::currentInputContext(mtm) {
            Some(ctx) => ctx.selectedKeyboardInputSource().map(|s| s.to_string()).unwrap_or("none".into()),
            None => "no-context".into(),
        }
    }

    pub fn post_key(code: u16, ch: &str, cmd: bool) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let app = NSApplication::sharedApplication(mtm);
        let Some(win) = app.keyWindow() else { return false };
        let flags = if cmd { NSEventModifierFlags::Command } else { NSEventModifierFlags::empty() };
        let _ = (ch, win, NSEventType::KeyDown, NSPoint::new(0.0, 0.0), NSProcessInfo::processInfo());
        for down in [true, false] {
            let Some(cg) = objc2_core_graphics::CGEvent::new_keyboard_event(None, code, down) else { return false };
            if cmd {
                objc2_core_graphics::CGEvent::set_flags(Some(&cg), objc2_core_graphics::CGEventFlags::MaskCommand);
            } else {
                objc2_core_graphics::CGEvent::set_flags(Some(&cg), objc2_core_graphics::CGEventFlags(0));
            }
            let _ = flags;
            match NSEvent::eventWithCGEvent(&cg) {
                Some(ev) => app.postEvent_atStart(&ev, false),
                None => return false,
            }
        }
        true
    }
}

#[derive(Clone)]
enum Step {
    Source(&'static str),
    Key(u16, &'static str, bool),
    Mark(&'static str),
    Wait(u64),
}

fn ime_script() -> std::collections::VecDeque<Step> {
    use Step::*;
    let k = |c, s| Key(c, s, false);
    vec![
        Source("com.apple.inputmethod.Korean.2SetKorean"),
        Wait(3000),
        Mark("korean selected"),
        k(5, "g"), k(40, "k"), k(1, "s"), k(15, "r"), k(46, "m"), k(3, "f"),
        Mark("typed gksrmf, expect 한글"),
        k(49, " "),
        k(2, "d"), k(46, "m"), k(0, "a"), k(2, "d"), k(40, "k"), k(15, "r"),
        Mark("typed dmadkr, expect 한글 음악"),
        k(51, "\u{7f}"),
        Mark("backspace during composition, expect 한글 음아"),
        k(15, "r"),
        Mark("retyped r, expect 한글 음악"),
        Source("com.apple.keylayout.ABC"),
        Mark("switched to ABC, composition must be committed"),
        k(0, "a"), k(11, "b"),
        Mark("typed ab, expect 한글 음악ab"),
        Wait(500),
        Key(0, "a", true),
        Key(8, "c", true),
        k(124, "\u{f703}"),
        Key(9, "v", true),
        Mark("select-all, copy, right, paste: expect doubled text"),
    ]
    .into()
}

struct Spike {
    ime_steps: std::collections::VecDeque<Step>,
    ime_next: std::time::Instant,
    search: String,
    volume: f32,
    shuffle: bool,
    events: Arc<Mutex<Vec<String>>>,
    rows: Vec<(String, String, String)>,
    selected: Option<usize>,
    frames: u64,
    _menu: muda::Menu,
    _remote: Vec<objc2::rc::Retained<objc2::runtime::AnyObject>>,
}

fn build_menu(events: Arc<Mutex<Vec<String>>>, ctx: egui::Context) -> muda::Menu {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    let menu = Menu::new();
    let app_m = Submenu::new("OnEum", true);
    let file_m = Submenu::new("파일", true);
    let edit_m = Submenu::new("편집", true);
    let win_m = Submenu::new("윈도우", true);
    menu.append_items(&[&app_m, &file_m, &edit_m, &win_m]).unwrap();
    app_m
        .append_items(&[
            &PredefinedMenuItem::about(None, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                "settings",
                "설정…",
                true,
                Some(Accelerator::new(Modifiers::META, Code::Comma)),
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])
        .unwrap();
    file_m
        .append_items(&[
            &MenuItem::with_id(
                "open",
                "열기…",
                true,
                Some(Accelerator::new(Modifiers::META, Code::KeyO)),
            ),
            &PredefinedMenuItem::close_window(None),
        ])
        .unwrap();
    edit_m
        .append_items(&[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ])
        .unwrap();
    win_m
        .append_items(&[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::fullscreen(None)])
        .unwrap();
    menu.init_for_nsapp();
    win_m.set_as_windows_menu_for_nsapp();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        log(&events, format!("menu event: {:?}", e.id));
        ctx.request_repaint();
    }));
    menu
}

fn setup_now_playing(
    events: Arc<Mutex<Vec<String>>>,
    ctx: egui::Context,
) -> Vec<objc2::rc::Retained<objc2::runtime::AnyObject>> {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
    use objc2_media_player::*;
    use std::ptr::NonNull;
    let mut tokens = Vec::new();
    unsafe {
        let cc = MPRemoteCommandCenter::sharedCommandCenter();
        let mk = |name: &'static str| {
            let events = events.clone();
            let ctx = ctx.clone();
            RcBlock::new(move |_e: NonNull<MPRemoteCommandEvent>| -> MPRemoteCommandHandlerStatus {
                log(&events, format!("remote command: {name}"));
                ctx.request_repaint();
                MPRemoteCommandHandlerStatus::Success
            })
        };
        for (cmd, name) in [
            (cc.playCommand(), "play"),
            (cc.pauseCommand(), "pause"),
            (cc.togglePlayPauseCommand(), "toggle"),
            (cc.nextTrackCommand(), "next"),
            (cc.previousTrackCommand(), "previous"),
        ] {
            cmd.setEnabled(true);
            tokens.push(cmd.addTargetWithHandler(&mk(name)));
        }
        let info = NSMutableDictionary::<NSString, AnyObject>::new();
        info.insert(MPMediaItemPropertyTitle, &*NSString::from_str("스파이크 곡 제목"));
        info.insert(MPMediaItemPropertyArtist, &*NSString::from_str("OnEum Spike"));
        info.insert(MPMediaItemPropertyAlbumTitle, &*NSString::from_str("검증 앨범"));
        info.insert(MPMediaItemPropertyPlaybackDuration, &*NSNumber::new_f64(180.0));
        info.insert(MPNowPlayingInfoPropertyElapsedPlaybackTime, &*NSNumber::new_f64(12.0));
        info.insert(MPNowPlayingInfoPropertyPlaybackRate, &*NSNumber::new_f64(1.0));
        let center = MPNowPlayingInfoCenter::defaultCenter();
        center.setNowPlayingInfo(Some(&info));
        center.setPlaybackState(MPNowPlayingPlaybackState::Playing);
        let back = center.nowPlayingInfo().map(|d| d.count()).unwrap_or(0);
        log(&events, format!("now playing info set, readback keys={back}, state={:?}", center.playbackState()));
    }
    tokens
}

impl eframe::App for Spike {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        use oneum::platform::macos::responder::{self, EditAction};
        for a in responder::drain() {
            log(&self.events, format!("edit action {a:?}"));
            let key = |key, shift| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { shift, command: true, mac_cmd: true, ..Default::default() },
            };
            raw.events.push(match a {
                EditAction::Cut => egui::Event::Cut,
                EditAction::Copy => egui::Event::Copy,
                EditAction::Paste => egui::Event::Paste(responder::pasteboard_text().unwrap_or_default()),
                EditAction::SelectAll => key(egui::Key::A, false),
                EditAction::Undo => key(egui::Key::Z, false),
                EditAction::Redo => key(egui::Key::Z, true),
            });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let want_focus = !self.ime_steps.is_empty();
        if want_focus {
            ui.input(|i| {
                for e in &i.events {
                    match e {
                        egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) | egui::Event::Ime(_) => {
                            log(&self.events, format!("egui event {e:?}"))
                        }
                        egui::Event::Key { key, pressed: true, modifiers, .. } if modifiers.command => {
                            log(&self.events, format!("egui cmd-key {key:?}"))
                        }
                        _ => {}
                    }
                }
            });
        }
        let mut extra = 0;
        if want_focus && std::time::Instant::now() >= self.ime_next {
            match self.ime_steps.pop_front() {
                Some(Step::Source(id)) => log(&self.events, format!("select {id}: {}", ime::select(id))),
                Some(Step::Key(c, s, cmd)) => {
                    if !ime::post_key(c, s, cmd) {
                        log(&self.events, format!("post_key {c} failed (no key window?)"));
                    }
                }
                Some(Step::Mark(m)) => log(&self.events, format!("CHECK {m} => {:?} [src={}]", self.search, ime::current())),
                Some(Step::Wait(ms)) => extra = ms,
                None => {}
            }
            self.ime_next = std::time::Instant::now() + std::time::Duration::from_millis(250 + extra);
        }
        if want_focus {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("검색:");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("제목, 아티스트, 앨범")
                        .desired_width(240.0),
                );
                if want_focus && !r.has_focus() {
                    r.request_focus();
                }
                if r.changed() {
                    log(&self.events, format!("search text = {:?}", self.search));
                }
                if ui.button("▶").on_hover_text("재생").clicked() {
                    log(&self.events, "play clicked".into());
                }
                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("볼륨"));
                ui.checkbox(&mut self.shuffle, "셔플");
                ui.label(format!("frames: {}", self.frames));
            });
        });
        egui::Panel::bottom("log").show(ui, |ui| {
            let ev = self.events.lock().unwrap();
            for line in ev.iter().rev().take(4) {
                ui.label(line);
            }
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let q = self.search.to_lowercase();
            let idx: Vec<usize> = if q.is_empty() {
                (0..self.rows.len()).collect()
            } else {
                (0..self.rows.len()).filter(|&i| self.rows[i].0.to_lowercase().contains(&q)).collect()
            };
            ui.label(format!("{} 곡", idx.len()));
            TableBuilder::new(ui)
                .striped(true)
                .sense(egui::Sense::click())
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::initial(200.0).clip(true))
                .column(Column::initial(200.0).clip(true))
                .header(22.0, |mut h| {
                    h.col(|ui| {
                        ui.strong("제목");
                    });
                    h.col(|ui| {
                        ui.strong("아티스트");
                    });
                    h.col(|ui| {
                        ui.strong("앨범");
                    });
                })
                .body(|body| {
                    body.rows(22.0, idx.len(), |mut row| {
                        let i = idx[row.index()];
                        row.set_selected(self.selected == Some(i));
                        let r = &self.rows[i];
                        row.col(|ui| {
                            ui.label(&r.0);
                        });
                        row.col(|ui| {
                            ui.label(&r.1);
                        });
                        row.col(|ui| {
                            ui.label(&r.2);
                        });
                        if row.response().clicked() {
                            self.selected = Some(i);
                        }
                    });
                });
        });
    }
}

fn main() -> eframe::Result {
    let events = Arc::new(Mutex::new(Vec::new()));
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 640.0]).with_title("OnEum Spike"),
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_default_menu(false);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "OnEum Spike",
        opts,
        Box::new(move |cc| {
            let menu = if std::env::var("SPIKE_NOMENU").is_ok() {
                muda::Menu::new()
            } else {
                build_menu(events.clone(), cc.egui_ctx.clone())
            };
            let c2 = cc.egui_ctx.clone();
            let ok = oneum::platform::macos::responder::install(move || c2.request_repaint());
            log(&events, format!("responder bridge installed: {ok}"));
            let remote = setup_now_playing(events.clone(), cc.egui_ctx.clone());
            let rows = (0..50_000)
                .map(|i| {
                    (
                        format!("곡 제목 {i} — Song Title 🎵 {i}"),
                        format!("아티스트 {}", i % 500),
                        format!("앨범 {}", i % 4000),
                    )
                })
                .collect();
            let ime_on = std::env::var("SPIKE_IME").is_ok();
            Ok(Box::new(Spike {
                ime_steps: if ime_on { ime_script() } else { Default::default() },
                ime_next: std::time::Instant::now() + std::time::Duration::from_secs(3),
                search: String::new(),
                volume: 0.8,
                shuffle: false,
                events,
                rows,
                selected: None,
                frames: 0,
                _menu: menu,
                _remote: remote,
            }))
        }),
    )
}
