//! The app's one scene.
//!
//! iOS ends an app at launch if it does not take part in the scene life cycle. winit
//! does not: it makes a window without a scene and listens to the application's
//! notifications. So the scene delegate is ours. It is named in the Info.plist, UIKit
//! makes it when the scene connects, and it puts winit's window into the scene. It is
//! also where the system hands over files to open and says that the app has left the
//! foreground or come back to it.
use super::{app_hooks, files, lifecycle};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, Message, define_class, ffi, sel};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSSet};
use objc2_ui_kit::{
    UIEdgeInsets, UIKeyboardFrameEndUserInfoKey, UIKeyboardWillChangeFrameNotification,
    UIOpenURLContext, UIResponder, UIScene, UISceneConnectionOptions, UISceneDelegate,
    UISceneSession, UIUserInterfaceStyle, UIView, UIWindow, UIWindowScene, UIWindowSceneDelegate,
};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

thread_local! {
    /// The connected scene (main thread only, like all of UIKit).
    static SCENE: RefCell<Option<Retained<UIWindowScene>>> = const { RefCell::new(None) };
    /// The view winit draws into.
    static VIEW: RefCell<Option<Retained<UIView>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `UIResponder` has no subclassing requirements; the class has no instance
    // variables and does not implement `Drop`.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OneumSceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(
            &self,
            scene: &UIScene,
            _session: &UISceneSession,
            options: &UISceneConnectionOptions,
        ) {
            if let Some(scene) = scene.downcast_ref::<UIWindowScene>() {
                SCENE.with(|slot| *slot.borrow_mut() = Some(scene.retain()));
                attach_window();
            }
            // Files the app was started with. (The bindings say this set is never
            // nil. It is, when there are none.)
            // SAFETY: a getter that returns an `NSSet<UIOpenURLContext *> *` or nil.
            let contexts: Option<Retained<NSSet<UIOpenURLContext>>> =
                unsafe { objc2::msg_send![options, URLContexts] };
            if let Some(contexts) = contexts {
                open(&contexts);
            }
        }

        #[unsafe(method(scene:openURLContexts:))]
        fn open_urls(&self, _scene: &UIScene, contexts: &NSSet<UIOpenURLContext>) {
            open(contexts);
        }

        #[unsafe(method(sceneDidEnterBackground:))]
        fn did_enter_background(&self, _scene: &UIScene) {
            lifecycle::entered_background();
        }

        #[unsafe(method(sceneWillEnterForeground:))]
        fn will_enter_foreground(&self, _scene: &UIScene) {
            lifecycle::entering_foreground();
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

/// Makes the delegate class known to the Objective-C runtime. UIKit looks it up by the
/// name in the Info.plist, so this has to happen before the app's run loop starts.
pub(super) fn register() {
    let _ = SceneDelegate::class();
    observe_keyboard();
}

/// How much of the screen's lower end the on-screen keyboard covers, in points (as
/// `f32` bits; 0 while it is away).
static KEYBOARD: AtomicU32 = AtomicU32::new(0);

pub(super) fn keyboard_height() -> f32 {
    f32::from_bits(KEYBOARD.load(Ordering::Relaxed))
}

/// Keeps [`keyboard_height`] up to date. The keyboard comes up over the app's view
/// without the view being resized: the app has to keep what matters above it.
fn observe_keyboard() {
    let block = RcBlock::new(|notification: NonNull<NSNotification>| {
        // SAFETY: the notification is valid for the duration of the call; the key is a
        // UIKit constant whose value is an `NSValue` holding a `CGRect`.
        let frame: Option<CGRect> = unsafe {
            notification
                .as_ref()
                .userInfo()
                .and_then(|info| info.objectForKey(UIKeyboardFrameEndUserInfoKey))
                .map(|value| objc2::msg_send![&*value, CGRectValue])
        };
        let Some(frame) = frame else {
            return;
        };
        // The frame is in screen coordinates: it starts at or below the screen's lower
        // edge when the keyboard is away.
        let screen = MainThreadMarker::new()
            .and_then(|_| VIEW.with(|slot| slot.borrow().as_ref()?.window()))
            .map_or(0.0, |window| window.bounds().size.height);
        let covered = (screen - frame.origin.y).max(0.0) as f32;
        if KEYBOARD.swap(covered.to_bits(), Ordering::Relaxed) != covered.to_bits() {
            app_hooks::wake();
        }
    });
    // SAFETY: a valid UIKit notification name; the block is copied by the center and
    // only touches process-global state.
    let token = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(UIKeyboardWillChangeFrameNotification),
            None,
            None,
            &block,
        )
    };
    // Observed for the whole process lifetime.
    std::mem::forget(token);
}

/// Remembers the view of winit's window, and puts the window into the scene if that is
/// connected already.
pub(super) fn remember_view(view: Retained<UIView>) {
    VIEW.with(|slot| *slot.borrow_mut() = Some(view));
    report_no_safe_area();
    attach_window();
}

extern "C-unwind" fn no_insets(_this: *mut AnyObject, _cmd: Sel) -> UIEdgeInsets {
    UIEdgeInsets {
        top: 0.0,
        left: 0.0,
        bottom: 0.0,
        right: 0.0,
    }
}

/// Makes winit's window say that it has no safe-area margins.
///
/// winit gives the window's size without those margins, and eframe makes its drawing
/// surface that size while it lays out and draws for the whole screen: the picture
/// comes out stretched and its lower end is cut off. With no margins to take away, both
/// sizes are the whole screen. The real margins are read from `UIWindow` itself
/// ([`safe_area_insets`]) and handed to egui by the app.
fn report_no_safe_area() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(cls) = AnyClass::get(c"WinitUIWindow") else {
        tracing::warn!("winit's window class is not there: the screen may be drawn stretched");
        return;
    };
    type Getter = extern "C-unwind" fn(*mut AnyObject, Sel) -> UIEdgeInsets;
    // SAFETY: `cls` is a registered class. The IMP has the signature of
    // `-[UIWindow safeAreaInsets]` (`UIEdgeInsets (id, SEL)`), which winit's subclass
    // does not implement itself, so nothing of winit's is replaced.
    unsafe {
        ffi::class_addMethod(
            std::ptr::from_ref(cls).cast_mut(),
            sel!(safeAreaInsets),
            std::mem::transmute::<Getter, Imp>(no_insets),
            c"{UIEdgeInsets=dddd}@:".as_ptr(),
        );
    }
}

/// The margins the system asks to keep free (status bar, Dynamic Island, home
/// indicator) as top, left, bottom, right in points. `None` before the window exists.
///
/// On an iPad (iPadOS 26 and later) the app runs in a window whose controls, three dots
/// at its top left, lie inside the safe area: over the back button and the title of a
/// page. The safe area "with corner adaptation" keeps them free too; vertically, so that
/// the page begins below them rather than everything moving aside. Only its top is
/// taken: elsewhere it also keeps clear of the rounded corners of the screen (16 points
/// at each side of an iPhone), which the app's pages need not, and which would take
/// the screen's left edge, where a finger pulls a page back.
pub(super) fn safe_area_insets() -> Option<[f32; 4]> {
    VIEW.with(|slot| {
        let view = slot.borrow();
        let view = view.as_ref()?;
        let window = view.window()?;
        // SAFETY: `UIWindow`'s own getter, not the override on winit's subclass above.
        let mut insets: UIEdgeInsets =
            unsafe { objc2::msg_send![super(&*window, UIWindow::class()), safeAreaInsets] };
        if let Some(adapted) = corner_adapted_insets(view) {
            insets.top = insets.top.max(adapted.top);
        }
        Some([
            insets.top as f32,
            insets.left as f32,
            insets.bottom as f32,
            insets.right as f32,
        ])
    })
}

thread_local! {
    /// `+[UIViewLayoutRegion safeAreaLayoutRegionWithCornerAdaptation:]` with the
    /// vertical axis, made once; `None` where the system has no such thing (before
    /// iOS 26).
    static ADAPTED_REGION: RefCell<Option<Option<Retained<AnyObject>>>> =
        const { RefCell::new(None) };
}

/// The insets of `view`'s safe area adapted to the window's corner controls (see
/// [`safe_area_insets`]), where the system knows of them.
fn corner_adapted_insets(view: &UIView) -> Option<UIEdgeInsets> {
    /// `UIViewLayoutRegionAdaptivityAxisVertical`.
    const VERTICAL: usize = 2;
    let region = ADAPTED_REGION.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                let class = AnyClass::get(c"UIViewLayoutRegion")?;
                // SAFETY: a class method of `UIViewLayoutRegion` (iOS 26) that takes an
                // `NSUInteger` and returns an autoreleased region.
                unsafe {
                    objc2::msg_send![class, safeAreaLayoutRegionWithCornerAdaptation: VERTICAL]
                }
            })
            .clone()
    })?;
    if !view.respondsToSelector(sel!(edgeInsetsForLayoutRegion:)) {
        return None;
    }
    // SAFETY: `-[UIView edgeInsetsForLayoutRegion:]` (iOS 26), which the view answers
    // to (checked above), with a region made by the class above.
    Some(unsafe { objc2::msg_send![view, edgeInsetsForLayoutRegion: &*region] })
}

/// The view controller to present another one from (the document picker).
pub(super) fn root_view_controller() -> Option<Retained<objc2_ui_kit::UIViewController>> {
    VIEW.with(|slot| {
        let view = slot.borrow();
        view.as_ref()?.window()?.rootViewController()
    })
}

thread_local! {
    /// The appearance the app asked for (see [`set_appearance`]), kept for a window
    /// that does not exist yet.
    static APPEARANCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether the phone is set to its dark appearance: what the scene says, which is the
/// system's setting whatever the window was told to look like.
pub(super) fn system_is_dark() -> Option<bool> {
    MainThreadMarker::new()?;
    SCENE.with(|slot| {
        let scene = slot.borrow();
        // SAFETY: a plain getter of the scene's trait collection.
        let style = unsafe { scene.as_ref()?.traitCollection().userInterfaceStyle() };
        match style {
            UIUserInterfaceStyle::Dark => Some(true),
            UIUserInterfaceStyle::Light => Some(false),
            _ => None,
        }
    })
}

/// Has what the system draws for the app's window be dark, light, or as the phone is
/// set (`None`): the status bar's text, and the sheets the system puts over the app.
pub(super) fn set_appearance(dark: Option<bool>) {
    APPEARANCE.with(|slot| slot.set(dark));
    apply_appearance();
}

fn apply_appearance() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let style = match APPEARANCE.with(std::cell::Cell::get) {
        Some(true) => UIUserInterfaceStyle::Dark,
        Some(false) => UIUserInterfaceStyle::Light,
        None => UIUserInterfaceStyle::Unspecified,
    };
    VIEW.with(|slot| {
        if let Some(window) = slot.borrow().as_ref().and_then(|view| view.window()) {
            window.setOverrideUserInterfaceStyle(style);
        }
    });
}

/// Puts winit's window into the scene, once both are known. Without a scene a window is
/// not shown.
fn attach_window() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    VIEW.with(|view| {
        SCENE.with(|scene| {
            let (view, scene) = (view.borrow(), scene.borrow());
            let (Some(view), Some(scene)) = (view.as_ref(), scene.as_ref()) else {
                return;
            };
            let Some(window) = view.window() else {
                tracing::warn!("the app's view is in no window: nothing can be shown");
                return;
            };
            if window.windowScene().is_none() {
                window.setWindowScene(Some(scene));
            }
            window.makeKeyAndVisible();
        });
    });
    apply_appearance();
}

/// Files handed over by the system: copied into the app's folder if they lie outside it,
/// then given to the app like files opened from Finder on a Mac.
fn open(contexts: &NSSet<UIOpenURLContext>) {
    let paths: Vec<_> = contexts
        .iter()
        .filter_map(|context| files::take_in(&context.URL()))
        .collect();
    app_hooks::receive(paths);
}
