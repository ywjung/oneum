//! The audio session: what kind of sound the app makes, and what the system does to it.
//!
//! The session is told that this is a music player ("playback"): it then plays on with
//! the screen off and with the silent switch set, and the system's controls (lock
//! screen, Control Center, headphone buttons) are the app's while it plays.
//!
//! cpal watches the session too, to keep its stream alive: it stops the stream when an
//! interruption begins and starts it again afterwards, and reports a changed route as an
//! invalidated stream, which the engine answers by opening a new one. What it cannot
//! know is what a music player owes its listener then: to be paused when a call comes
//! in (and to show that it is), and to stay silent when the headphones are pulled out
//! instead of playing on through the speaker. Those are told to the app from here.
use crate::platform::SoundEvent;
use block2::RcBlock;
use objc2_avf_audio::{
    AVAudioSession, AVAudioSessionCategoryPlayback, AVAudioSessionInterruptionNotification,
    AVAudioSessionInterruptionOptionKey, AVAudioSessionInterruptionOptions,
    AVAudioSessionInterruptionType, AVAudioSessionInterruptionTypeKey,
    AVAudioSessionRouteChangeNotification, AVAudioSessionRouteChangeReason,
    AVAudioSessionRouteChangeReasonKey,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNumber, NSString};
use std::ptr::NonNull;
use std::sync::Arc;

/// Declares the app a music player and makes its session the active one. Called before
/// an output stream is opened; safe to call again. False if the system refused (another
/// app's call is on, for example): the stream may then not start.
pub fn activate() -> bool {
    // SAFETY: the shared session, a documented category constant, and plain setters.
    unsafe {
        let session = AVAudioSession::sharedInstance();
        if let Some(category) = AVAudioSessionCategoryPlayback
            && let Err(error) = session.setCategory_error(category)
        {
            tracing::warn!("the audio session refused the playback category: {error}");
        }
        match session.setActive_error(true) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!("the audio session could not be made active: {error}");
                false
            }
        }
    }
}

/// Asked right before sound is made. The session was made the active one when the
/// stream was opened ([`activate`]): there is nothing more to ask for here.
pub fn claim() -> bool {
    true
}

/// The player has answered the end of an interruption. Nothing depends on it here.
pub fn interruption_settled(_resumed: bool) {}

/// The number stored under `key` in a notification's `userInfo`.
fn number(notification: &NSNotification, key: Option<&NSString>) -> Option<usize> {
    let info = notification.userInfo()?;
    let value = info.objectForKey(key?)?;
    Some(value.downcast::<NSNumber>().ok()?.unsignedIntegerValue())
}

/// Calls `on_event` (on the thread the system posts on) when the session is
/// interrupted, when the interruption is over, and when the output the sound went to
/// is gone. Observed for as long as the process lives.
pub fn observe(on_event: impl Fn(SoundEvent) + Send + Sync + 'static) {
    let on_event = Arc::new(on_event);
    let center = NSNotificationCenter::defaultCenter();

    let send = on_event.clone();
    let interruption = RcBlock::new(move |notification: NonNull<NSNotification>| {
        // SAFETY: the notification is valid for the duration of the call; the keys are
        // AVFAudio's documented constants.
        let (kind, options) = unsafe {
            let notification = notification.as_ref();
            (
                number(notification, AVAudioSessionInterruptionTypeKey),
                number(notification, AVAudioSessionInterruptionOptionKey).unwrap_or(0),
            )
        };
        let Some(kind) = kind else {
            return;
        };
        if AVAudioSessionInterruptionType(kind) == AVAudioSessionInterruptionType::Began {
            send(SoundEvent::Interrupted);
        } else {
            let resume = AVAudioSessionInterruptionOptions(options)
                .contains(AVAudioSessionInterruptionOptions::ShouldResume);
            send(SoundEvent::InterruptionOver { resume });
        }
    });
    let send = on_event;
    let route = RcBlock::new(move |notification: NonNull<NSNotification>| {
        // SAFETY: as above.
        let reason = unsafe { number(notification.as_ref(), AVAudioSessionRouteChangeReasonKey) };
        if reason.map(AVAudioSessionRouteChangeReason)
            == Some(AVAudioSessionRouteChangeReason::OldDeviceUnavailable)
        {
            send(SoundEvent::OutputGone);
        }
    });
    // SAFETY: valid notification names; the blocks are copied by the center and only
    // call a `Send + Sync` closure.
    unsafe {
        for (name, block) in [
            (AVAudioSessionInterruptionNotification, &interruption),
            (AVAudioSessionRouteChangeNotification, &route),
        ] {
            if let Some(name) = name {
                let token = center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    None,
                    block,
                );
                std::mem::forget(token);
            }
        }
    }
}
