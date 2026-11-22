//! TalkBack: the accessibility tree of the UI library, told to a view of the Java
//! shell (android/…/A11yHost.java), and what TalkBack asks of it brought back.
//!
//! eframe hands the tree to Android only with the "game activity", which the app does
//! not use. The UI library makes the tree all the same once it is asked to; a plugin
//! of the library takes it from each frame's output ([`Bridge`]), flattened
//! (`ui::accessible`), and gives it to Java when it changed. Java asks for actions by
//! a node's number (a tap on a button: a click); they go into the next frame's input
//! as the library's own action requests.
//!
//! Only while the system says accessibility is on (TalkBack, Switch Access, a tool
//! that reads the screen): the tree costs work every frame.
use crate::ui::accessible::{Mirror, actions_for};
use eframe::egui;
use egui::accesskit::{ActionRequest, TreeId};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The system says an accessibility service is on.
static ON: AtomicBool = AtomicBool::new(false);
/// What TalkBack asked for since the last frame: a node's number and the action's
/// (see `ui::accessible::actions_for`).
static ASKED: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());

/// The system's word whether an accessibility service is on (see `natives`).
pub(super) fn set_on(on: bool) {
    ON.store(on, Ordering::Release);
    tracing::info!(on, "accessibility");
    super::app_hooks::wake();
}

/// TalkBack asked `action` of the node numbered `id` (see `natives`).
pub(super) fn asked(id: i32, action: i32) {
    if let Ok(mut asked) = ASKED.lock() {
        // (A queue that nobody takes from, the window being away, stays small.)
        if asked.len() < 64 {
            asked.push((id, action));
        }
    }
    super::app_hooks::wake();
}

/// What the nodes Java was told last say, in their order (for verification).
static TOLD_TEXTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What the nodes of the accessibility tree say, as Java was told them last.
pub fn told_texts() -> Vec<String> {
    TOLD_TEXTS
        .lock()
        .map(|texts| texts.clone())
        .unwrap_or_default()
}

/// What the node TalkBack is on says (for verification: `talkback_on`).
static TALKBACK_ON: Mutex<Option<String>> = Mutex::new(None);

/// TalkBack moved to a node of the app's, or off them (see `natives`).
pub(super) fn focused(said: Option<String>) {
    if let Ok(mut on) = TALKBACK_ON.lock() {
        *on = said;
    }
}

/// What the node TalkBack is on says, where it is on one of the app's.
pub fn talkback_on() -> Option<String> {
    TALKBACK_ON.lock().ok().and_then(|on| on.clone())
}

/// For verification: asks of the node that says `said` what TalkBack would (moving
/// onto it, and with `tap` a double tap). The Java side logs what came of it.
#[cfg(any(debug_assertions, feature = "automation"))]
pub fn ask(said: &str, tap: bool) {
    super::java::accessibility_ask(said, if tap { 2 } else { 1 });
}

/// The plugin of the UI library that does the work (see the module).
#[derive(Default)]
pub struct Bridge {
    mirror: Mirror,
    /// Whether the library was asked for the tree.
    making: bool,
    /// What Java was told last.
    told: String,
    /// The frames since accessibility came on, and whether a tree came of them.
    frames: u32,
    trees: u32,
}

impl egui::Plugin for Bridge {
    fn debug_name(&self) -> &'static str {
        "oneum-accessibility"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let on = ON.load(Ordering::Acquire);
        if on != self.making {
            tracing::info!(on, "accessibility: the tree is asked for");
            if on {
                ctx.enable_accesskit();
            } else {
                ctx.disable_accesskit();
                *self = Bridge::default();
            }
            self.making = on;
        }
        let asked = ASKED
            .lock()
            .map(|mut asked| std::mem::take(&mut *asked))
            .unwrap_or_default();
        for (id, action) in asked {
            let Some(node) = self.mirror.node_of(id) else {
                continue;
            };
            for action in actions_for(action) {
                input
                    .events
                    .push(egui::Event::AccessKitActionRequest(ActionRequest {
                        action: *action,
                        target_tree: TreeId::ROOT,
                        target_node: node,
                        data: None,
                    }));
            }
        }
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        if !self.making {
            return;
        }
        self.frames = self.frames.saturating_add(1);
        let Some(update) = &output.platform_output.accesskit_update else {
            if self.frames == 3 && self.trees == 0 {
                tracing::warn!("accessibility: the UI library makes no tree");
            }
            return;
        };
        self.trees = self.trees.saturating_add(1);
        self.mirror.apply(update);
        let flat = self.mirror.flatten();
        let Ok(json) = serde_json::to_string(&flat) else {
            return;
        };
        if json != self.told {
            if self.told.is_empty() {
                tracing::info!(nodes = flat.nodes.len(), "accessibility: the tree is told");
            }
            super::java::accessibility_tree(&json);
            self.told = json;
            if let Ok(mut texts) = TOLD_TEXTS.lock() {
                *texts = flat
                    .nodes
                    .iter()
                    .filter(|node| !node.text.is_empty() || !node.hint.is_empty())
                    .map(|node| {
                        if node.text.is_empty() {
                            format!("hint:{}", node.hint)
                        } else {
                            node.text.clone()
                        }
                    })
                    .collect();
            }
        }
    }
}
