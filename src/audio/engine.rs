//! Ties feeds, the renderer and the output device together. Owned by the player thread.
use super::PlaybackError;
use super::crossfeed::Crossfeed;
use super::decoder::SourceInfo;
use super::dsp::{ReplayGainSettings, volume_to_gain};
use super::eq::{EqSettings, EqShared};
use super::feed::{FeedEvent, FeedHandle, FeedParams, NextSlot, RgShared, TrackRequest};
use super::output::{
    self, DeviceHold, OutputError, OutputSettings, OutputSignals, OutputStream, choose_rate,
};
use super::peq::{PeqProfile, PeqShared};
use super::render::{RenderLink, RenderShared, Renderer, render_pair};
use crate::model::TrackId;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Audio that must be buffered before a new feed is handed to the callback.
const PREBUFFER_SECONDS: f64 = 0.12;
/// A silent stream is stopped after this long so the device and the Mac can sleep.
const IDLE_STOP: Duration = Duration::from_secs(3);
const DEVICE_POLL: Duration = Duration::from_secs(1);
/// Polls in a row that must miss the device before it counts as disconnected.
const DEVICE_MISSING_POLLS: u8 = 2;
/// Longest wait for the device to play out what it has already been given before the
/// stream is closed at the end of a feed (see `Engine::let_tail_play_out`).
const TAIL_MAX: Duration = Duration::from_millis(500);
/// A close this soon after a feed drained is the player reacting to that drain.
const TAIL_WINDOW: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Decode side: this track begins at `at_frame` of feed `epoch`.
    TrackStarted {
        epoch: u16,
        item_id: u64,
        track_id: TrackId,
        at_frame: u64,
        start_ms: u64,
        info: SourceInfo,
        /// What [`Engine::limited_frames`] stood at when the track began.
        limited_before: u64,
    },
    TrackFailed {
        epoch: u16,
        item_id: u64,
        track_id: TrackId,
        error: PlaybackError,
    },
    /// The decode thread finished; the feed is `total_frames` long.
    FeedFinished { epoch: u16, total_frames: u64 },
    /// Everything in feed `epoch` has been played.
    Drained { epoch: u16 },
    /// The output device disappeared. The engine has closed its stream.
    DeviceLost,
    /// The stream must be reopened (format change, or the system default moved to another
    /// device that is still connected). The engine has closed its stream.
    OutputChanged,
}

enum Sink {
    Closed,
    Device(Box<OutputStream>),
    /// Test/offline mode: the caller owns the [`Renderer`] and pulls audio from it.
    Offline(RenderLink),
}

pub struct Engine {
    shared: Arc<RenderShared>,
    rg: Arc<RgShared>,
    /// Frames whose gain the peak limiter turned down, counted by the decode threads.
    limited: Arc<AtomicU64>,
    eq: Arc<EqShared>,
    crossfeed: Arc<AtomicU8>,
    peq: Arc<PeqShared>,
    /// How fast the feeds started from now on go (see `FeedParams::speed`).
    speed: f32,
    /// The room correction of the feeds started from now on (see `FeedParams::room`).
    room: Option<Arc<super::room::Impulse>>,
    sink: Sink,
    /// `None` = follow the system default device.
    device_pref: Option<String>,
    feed_tx: Sender<FeedEvent>,
    feed_rx: Receiver<FeedEvent>,
    /// Written by the error callback of the stream that is open now. Replaced with every
    /// new stream so that a late error of an old stream cannot touch the new one.
    signals: Arc<OutputSignals>,
    /// Consecutive device polls that did not find the device.
    device_missing_polls: u8,
    /// Feed whose consumer has been handed to the renderer.
    active: Option<FeedHandle>,
    /// Feed still pre-buffering.
    loading: Option<FeedHandle>,
    last_epoch: u16,
    reported_drain: u16,
    idle_since: Option<Instant>,
    last_device_poll: Instant,
    settings: OutputSettings,
    /// Our claim on the device (exclusive access, changed format) while the settings ask
    /// for one. Outlives the stream when only the rate changes between two tracks, and
    /// is dropped, which puts the device back, whenever the output is really closed.
    hold: Option<DeviceHold>,
    /// Hardware sample format `(bits, is_float)` read when the stream was opened.
    hardware_bits: Option<(u32, bool)>,
    /// When the feed that was playing ran out (cleared when the next one starts).
    drained_at: Option<Instant>,
    /// `(match_sample_rate, exclusive)` the open stream was set up for.
    opened_use: (bool, bool),
    /// Rate the device would not switch to for the stream that is open now.
    refused_rate: Option<u32>,
    /// Offline engines only: rates a pretend device offers, so that the rate-matching
    /// decisions can be tested without hardware.
    offline_rates: Vec<(u32, u32)>,
}

/// What the output is doing, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStatus {
    pub device_name: String,
    pub sample_rate: u32,
    /// Hardware sample format `(bits, is_float)`, when the device tells.
    pub hardware_bits: Option<(u32, bool)>,
    /// This app has exclusive access to the device.
    pub exclusive: bool,
}

impl Engine {
    pub fn new(device_pref: Option<String>, volume: f32, rg: ReplayGainSettings) -> Self {
        let (feed_tx, feed_rx) = channel();
        Self {
            shared: RenderShared::new(volume_to_gain(volume)),
            rg: RgShared::new(rg),
            limited: Arc::new(AtomicU64::new(0)),
            eq: EqShared::new(EqSettings::default()),
            crossfeed: Arc::new(AtomicU8::new(0)),
            speed: 1.0,
            room: None,
            peq: PeqShared::new(None),
            sink: Sink::Closed,
            device_pref,
            feed_tx,
            feed_rx,
            signals: OutputSignals::new(),
            device_missing_polls: 0,
            active: None,
            loading: None,
            last_epoch: 0,
            reported_drain: 0,
            idle_since: None,
            last_device_poll: Instant::now(),
            settings: OutputSettings::default(),
            hold: None,
            hardware_bits: None,
            drained_at: None,
            opened_use: (false, false),
            refused_rate: None,
            offline_rates: Vec::new(),
        }
    }

    pub fn output_settings(&self) -> OutputSettings {
        self.settings
    }

    /// Changes how the device is used. Device-related changes take effect at the next
    /// [`Engine::open_output_for`]; memory playback at the next feed.
    pub fn set_output_settings(&mut self, settings: OutputSettings) {
        self.settings = settings;
    }

    /// Reproduces what the backend does when the device announces a format change: the
    /// stream is stopped behind the engine's back and an invalidation is signalled. Used
    /// by `oneum-libtool audio-selftest` to check on a real device that the engine
    /// recovers when such a notification arrives late for a change it made itself.
    pub fn simulate_format_notification(&mut self) {
        if let Sink::Device(stream) = &mut self.sink {
            stream.stop_like_the_backend();
        }
        self.signals.signal_invalidated();
    }

    /// Offline engines: pretend the device has this hardware sample format
    /// (`(bits, is_float)`), so that the dither decisions can be tested without hardware.
    pub fn set_offline_hardware_bits(&mut self, format: Option<(u32, bool)>) {
        self.hardware_bits = format;
        self.shared.set_dither_bits(match format {
            Some((bits, false)) => Some(bits),
            _ => None,
        });
    }

    /// Hardware sample format `(bits, is_float)` of the open output, when known.
    pub fn hardware_bits(&self) -> Option<(u32, bool)> {
        self.hardware_bits
    }

    /// Offline engines: pretend the device offers exactly these rates.
    pub fn set_offline_rates(&mut self, rates: &[u32]) {
        self.offline_rates = rates.iter().map(|r| (*r, *r)).collect();
    }

    /// True while the device is claimed (exclusive access or a changed format). Such a
    /// claim should not outlive playback: see [`Engine::close_output`].
    pub fn holds_device(&self) -> bool {
        self.hold.is_some()
    }

    /// Id of the device the next stream will be opened on.
    fn target_device_id(&self) -> Option<String> {
        // A claimed device stays ours until it is released: with exclusive access the
        // system may move its default elsewhere, and following that would be a loop.
        self.hold
            .as_ref()
            .map(|h| h.device_id.clone())
            .or_else(|| self.device_pref.clone())
            .or_else(output::default_output_id)
    }

    /// The rate the device should run at for a track of `source_rate` Hz. `None` when
    /// the device is to be left alone: rate matching is off, the track's rate is not
    /// known, or the device cannot be asked.
    pub fn preferred_rate(&self, source_rate: Option<u32>) -> Option<u32> {
        if !self.settings.match_sample_rate && !self.settings.upsample {
            return None;
        }
        let source = source_rate.filter(|r| *r > 0)?;
        let ranges = match &self.sink {
            Sink::Offline(_) => self.offline_rates.clone(),
            _ => match &self.hold {
                Some(hold) => hold.rate_ranges(),
                None => output::device_rate_ranges(&self.target_device_id()?),
            },
        };
        // Upsampling: a whole multiple of the track's rate, as high as the device
        // goes. A device that has none is run as for a track that is not upsampled:
        // at the track's rate where that is asked for, and left alone otherwise.
        self.settings
            .upsample
            .then(|| output::choose_raised_rate(source, &ranges))
            .flatten()
            .or_else(|| {
                self.settings
                    .match_sample_rate
                    .then(|| choose_rate(source, &ranges))
                    .flatten()
            })
    }

    pub fn output_status(&self) -> Option<OutputStatus> {
        match &self.sink {
            Sink::Device(stream) => Some(OutputStatus {
                device_name: stream.device_name.clone(),
                sample_rate: stream.sample_rate(),
                hardware_bits: self.hardware_bits,
                exclusive: self.hold.as_ref().is_some_and(DeviceHold::is_exclusive),
            }),
            _ => None,
        }
    }

    /// Engine without a device: the returned renderer stands in for the output callback.
    pub fn new_offline(sample_rate: u32, volume: f32, rg: ReplayGainSettings) -> (Self, Renderer) {
        let mut engine = Self::new(None, volume, rg);
        let (renderer, link) = render_pair(engine.shared.clone(), sample_rate);
        engine.sink = Sink::Offline(link);
        (engine, renderer)
    }

    /// The signals of the current stream. Tests use this to stand in for the device's
    /// error callback.
    pub fn output_signals(&self) -> Arc<OutputSignals> {
        self.signals.clone()
    }

    pub fn shared(&self) -> &Arc<RenderShared> {
        &self.shared
    }

    fn link_mut(&mut self) -> Option<&mut RenderLink> {
        match &mut self.sink {
            Sink::Closed => None,
            Sink::Device(stream) => Some(&mut stream.link),
            Sink::Offline(link) => Some(link),
        }
    }

    pub fn sample_rate(&self) -> Option<u32> {
        match &self.sink {
            Sink::Closed => None,
            Sink::Device(stream) => Some(stream.sample_rate()),
            Sink::Offline(link) => Some(link.sample_rate),
        }
    }

    pub fn is_output_open(&self) -> bool {
        !matches!(self.sink, Sink::Closed)
    }

    /// `(id, name)` of the device the stream is open on.
    pub fn output_device(&self) -> Option<(&str, &str)> {
        match &self.sink {
            Sink::Device(stream) => Some((&stream.device_id, &stream.device_name)),
            _ => None,
        }
    }

    pub fn device_pref(&self) -> Option<&str> {
        self.device_pref.as_deref()
    }

    /// Changes the preferred device. Takes effect at the next [`Engine::open_output`].
    pub fn set_device_pref(&mut self, pref: Option<String>) {
        self.device_pref = pref;
    }

    /// Opens the output stream if it is not open (see [`Engine::open_output_for`]).
    pub fn open_output(&mut self) -> Result<(), OutputError> {
        self.open_output_for(None)
    }

    /// Makes the output ready for a track of `source_rate` Hz: opens the stream if it is
    /// not open, and with rate matching on, reopens it when the device should run at
    /// another rate for this track. Existing feeds are dropped whenever the stream is
    /// (re)opened, because the new one may run at a different sample rate.
    pub fn open_output_for(&mut self, source_rate: Option<u32>) -> Result<(), OutputError> {
        if matches!(self.sink, Sink::Offline(_)) {
            return Ok(());
        }
        let target = self.preferred_rate(source_rate);
        let wanted_use = (self.settings.match_sample_rate, self.settings.exclusive);
        if let Sink::Device(stream) = &self.sink {
            // A rate the device refused is not asked for again with every seek.
            let rate_fits = target
                .is_none_or(|rate| rate == stream.sample_rate() || Some(rate) == self.refused_rate);
            if rate_fits && self.opened_use == wanted_use {
                return Ok(());
            }
        }
        self.close_stream();
        self.refused_rate = None;
        self.opened_use = wanted_use;
        let device_id = self.target_device_id().ok_or(OutputError::NoDevice)?;
        if !self.settings.touches_device() {
            self.hold = None;
        } else {
            match &mut self.hold {
                Some(hold) => hold.set_exclusive(self.settings.exclusive),
                None => self.hold = DeviceHold::acquire(&device_id, self.settings.exclusive),
            }
        }
        if let (Some(hold), Some(rate)) = (&mut self.hold, target)
            && !hold.apply_rate(rate)
        {
            tracing::warn!(rate, "the output device did not switch its sample rate");
            self.refused_rate = Some(rate);
        }
        let signals = OutputSignals::new();
        let stream =
            match OutputStream::open(Some(&device_id), self.shared.clone(), signals.clone()) {
                Ok(stream) => stream,
                Err(error) => {
                    // Nothing is playing through it: do not keep the device claimed.
                    self.hold = None;
                    return Err(error);
                }
            };
        self.signals = signals;
        self.device_missing_polls = 0;
        self.hardware_bits = match &self.hold {
            Some(hold) => hold.hardware_bits(),
            None => output::device_hardware_bits(&device_id),
        };
        // An integer device gets dither for whatever is not passed through untouched.
        self.shared.set_dither_bits(match self.hardware_bits {
            Some((bits, false)) => Some(bits),
            _ => None,
        });
        tracing::info!(
            rate = stream.sample_rate(),
            channels = stream.channels,
            exclusive = self.hold.as_ref().is_some_and(DeviceHold::is_exclusive),
            "output stream opened"
        );
        self.sink = Sink::Device(Box::new(stream));
        self.idle_since = Some(Instant::now());
        Ok(())
    }

    /// Closes the stream but keeps the claim on the device (the stream is about to be
    /// reopened on it).
    fn close_stream(&mut self) {
        self.let_tail_play_out();
        self.stop_feeds();
        if matches!(self.sink, Sink::Device(_)) {
            self.sink = Sink::Closed;
            // No callback runs any more: the meter would keep its last reading.
            self.shared.meter().reset();
            self.shared.scope().reset();
            self.shared.set_dither_bits(None);
        }
    }

    /// A feed has "drained" when the callback took its last frame, but the device still
    /// holds that audio for its output latency. Closing the stream right then (to switch
    /// the rate for the next track, or to give the device back at the end of the queue)
    /// would cut the last moments of the track off, so wait them out first.
    fn let_tail_play_out(&mut self) {
        let Some(drained) = self.drained_at.take() else {
            return;
        };
        if drained.elapsed() > TAIL_WINDOW {
            return;
        }
        if let Sink::Device(stream) = &self.sink
            && stream.is_running()
        {
            let seconds =
                f64::from(self.shared.latency_frames()) / f64::from(stream.sample_rate().max(1));
            let tail = Duration::from_secs_f64(seconds.clamp(0.0, TAIL_MAX.as_secs_f64()));
            std::thread::sleep(tail.saturating_sub(drained.elapsed()));
        }
    }

    /// Closes the output and lets go of the device: exclusive access is given back and
    /// a changed sample rate or format is put back the way it was.
    pub fn close_output(&mut self) {
        // The stream goes first; the device is only reconfigured once nothing plays.
        self.close_stream();
        self.hold = None;
        self.hardware_bits = None;
    }

    /// Starts a new feed at `start_ms` of `first`. Audio of the previous feed keeps playing
    /// until the new one has pre-buffered, then the callback cross-switches with a short fade.
    pub fn start_feed(
        &mut self,
        first: TrackRequest,
        start_ms: u64,
        next: Arc<NextSlot>,
    ) -> Result<u16, OutputError> {
        let Some(out_rate) = self.sample_rate() else {
            return Err(OutputError::NoDevice);
        };
        self.last_epoch = if self.last_epoch == u16::MAX {
            1
        } else {
            self.last_epoch + 1
        };
        let epoch = self.last_epoch;
        let params = FeedParams {
            epoch,
            out_rate,
            first,
            start_ms,
            next,
            rg: self.rg.clone(),
            events: self.feed_tx.clone(),
            memory: self.settings.memory_playback,
            limited: self.limited.clone(),
            eq: self.eq.clone(),
            crossfeed: self.crossfeed.clone(),
            peq: self.peq.clone(),
            speed: self.speed,
            filter: self.settings.filter,
            room: self.room.clone(),
        };
        let handle = FeedHandle::spawn(params)
            .ok_or_else(|| OutputError::Backend("cannot spawn decode thread".into()))?;
        self.drained_at = None;
        self.loading = Some(handle); // replaces (and cancels) a feed that was still loading
        // Silence whatever is playing right now; only the new feed may be heard. The old
        // feed is abandoned: its decode thread stops and its remaining events are stale.
        self.shared.require_epoch(epoch);
        self.active = None;
        Ok(epoch)
    }

    /// Drops all feeds. The callback keeps the last consumer until it is replaced, but
    /// with `playing` false it stays silent.
    pub fn stop_feeds(&mut self) {
        self.loading = None;
        self.active = None;
    }

    pub fn active_epoch(&self) -> Option<u16> {
        self.active.as_ref().map(|f| f.epoch)
    }

    pub fn loading_epoch(&self) -> Option<u16> {
        self.loading.as_ref().map(|f| f.epoch)
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.shared.set_playing(playing);
        if playing {
            self.wake_stream();
        } else {
            self.idle_since = Some(Instant::now());
        }
    }

    fn wake_stream(&mut self) {
        self.idle_since = if self.shared.is_playing() {
            None
        } else {
            Some(Instant::now())
        };
        if let Sink::Device(stream) = &mut self.sink
            && let Err(e) = stream.set_running(true)
        {
            tracing::warn!("could not start output stream: {e}");
            self.signals.signal_lost();
        }
    }

    /// Tells the output whether the samples being decoded are still the file's own
    /// (see `RenderShared::set_processed`).
    pub fn set_processed(&self, processed: bool) {
        self.shared.set_processed(processed);
    }

    /// Width of the integer format dither is being added for, while it is.
    pub fn dither_bits(&self) -> Option<u32> {
        match self.hardware_bits {
            Some((bits, false)) if self.shared.dither_active() => Some(bits),
            _ => None,
        }
    }

    /// `volume` is the 0..=1 slider value.
    pub fn set_volume(&self, volume: f32) {
        self.shared.set_volume_gain(volume_to_gain(volume));
    }

    pub fn set_replaygain(&self, settings: ReplayGainSettings) {
        self.rg.set(settings);
    }

    /// Sets the equalizer. What is playing glides to it; it is heard once the audio
    /// already decoded (up to a second) has been played.
    pub fn set_eq(&self, settings: EqSettings) {
        self.eq.set(settings);
    }

    pub fn eq(&self) -> EqSettings {
        self.eq.get()
    }

    /// Sets the parametric correction profile (`None`: none). Like the equalizer it is
    /// cross-faded to and heard once the audio already decoded has been played.
    pub fn set_correction(&self, profile: Option<PeqProfile>) {
        self.peq.set(profile);
    }

    /// Sets the headphone crossfeed. Like the equalizer it is cross-faded to and heard
    /// once the audio already decoded has been played.
    pub fn set_crossfeed(&self, setting: Crossfeed) {
        self.crossfeed.store(setting.to_u8(), Ordering::Relaxed);
    }

    /// Sets how fast songs go, as a multiple of their own speed. It holds for the
    /// feeds started after this: the one that is playing goes on as it was started.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    /// Sets the room correction (or none). Like the speed it holds for the feeds
    /// started after this.
    pub fn set_room(&mut self, room: Option<Arc<super::room::Impulse>>) {
        self.room = room;
    }

    /// `(epoch, frames)` played by the callback, compensated for output latency.
    pub fn played(&self) -> (u16, u64) {
        let (epoch, frames) = self.shared.position();
        (
            epoch,
            frames.saturating_sub(u64::from(self.shared.latency_frames())),
        )
    }

    /// Frames of the active feed that are decoded but not yet played, and whether its
    /// decode thread has finished. `None` while no feed is active in the renderer.
    pub fn active_buffered(&self) -> Option<(u64, bool)> {
        let feed = self.active.as_ref()?;
        let (epoch, consumed) = self.shared.position();
        let consumed = if epoch == feed.epoch { consumed } else { 0 };
        Some((
            feed.written_frames().saturating_sub(consumed),
            feed.is_finished(),
        ))
    }

    /// True when feed `epoch` has decoded audio waiting to be played (or is complete).
    /// If nothing is heard in that situation, the output side is at fault, not the file.
    pub fn feed_has_audio(&self, epoch: u16) -> bool {
        if let Some(feed) = self.active.as_ref().filter(|f| f.epoch == epoch) {
            let (played_epoch, consumed) = self.shared.position();
            let consumed = if played_epoch == epoch { consumed } else { 0 };
            return feed.is_finished() || feed.written_frames() > consumed;
        }
        self.loading
            .as_ref()
            .is_some_and(|f| f.epoch == epoch && (f.is_finished() || f.written_frames() > 0))
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns()
    }

    /// Frames decoded so far whose gain the peak limiter turned down because they
    /// would have exceeded full scale (see `limiter.rs`). Counted where the audio is
    /// decoded, up to a second before it is heard.
    pub fn limited_frames(&self) -> u64 {
        self.limited.load(Ordering::Relaxed)
    }

    /// Housekeeping; call every ~20 ms. Appends events for the controller.
    pub fn poll(&mut self, events: &mut Vec<EngineEvent>) {
        // Free feeds the callback has finished with (deallocation happens here, not in the callback).
        if let Some(link) = self.link_mut() {
            while link.garbage.pop().is_ok() {}
        }

        // Promote the loading feed once it has enough audio (or is already complete).
        let rate = self.sample_rate().unwrap_or(48_000);
        let ready = self.loading.as_ref().is_some_and(|f| {
            f.is_finished() || f.written_frames() as f64 >= f64::from(rate) * PREBUFFER_SECONDS
        });
        if ready && let Some(mut feed) = self.loading.take() {
            let mut handed_over = false;
            if let Some(consumer) = feed.take_consumer() {
                match self.link_mut().map(|link| link.incoming.push(consumer)) {
                    Some(Ok(())) => handed_over = true,
                    Some(Err(rtrb::PushError::Full(consumer))) => feed.put_back_consumer(consumer),
                    None => {}
                }
            }
            if handed_over {
                self.active = Some(feed);
                self.wake_stream();
            } else {
                self.loading = Some(feed); // retry on the next poll
            }
        }

        while let Ok(event) = self.feed_rx.try_recv() {
            let current = |epoch: u16| {
                self.active.as_ref().is_some_and(|f| f.epoch == epoch)
                    || self.loading.as_ref().is_some_and(|f| f.epoch == epoch)
            };
            // Events of abandoned feeds are stale by definition.
            match event {
                FeedEvent::TrackStarted {
                    epoch,
                    item_id,
                    track_id,
                    at_frame,
                    start_ms,
                    info,
                    limited_before,
                } if current(epoch) => {
                    events.push(EngineEvent::TrackStarted {
                        epoch,
                        item_id,
                        track_id,
                        at_frame,
                        start_ms,
                        info,
                        limited_before,
                    });
                }
                FeedEvent::TrackFailed {
                    epoch,
                    item_id,
                    track_id,
                    error,
                } if current(epoch) => {
                    events.push(EngineEvent::TrackFailed {
                        epoch,
                        item_id,
                        track_id,
                        error,
                    });
                }
                FeedEvent::Finished {
                    epoch,
                    total_frames,
                } if current(epoch) => {
                    events.push(EngineEvent::FeedFinished {
                        epoch,
                        total_frames,
                    });
                }
                _ => {}
            }
        }

        let drained = self.shared.drained_epoch();
        if drained != 0 && drained != self.reported_drain && self.active_epoch() == Some(drained) {
            self.reported_drain = drained;
            self.drained_at = Some(Instant::now());
            events.push(EngineEvent::Drained { epoch: drained });
        }

        let signal = self.signals.take();
        if signal.other_errors > 0 {
            tracing::debug!(count = signal.other_errors, "output backend errors");
        }
        // Only a real device can tell us whether it is still there; offline sinks trust the signal.
        let device_id = match &self.sink {
            Sink::Device(stream) => Some(stream.device_id.clone()),
            _ => None,
        };
        if signal.lost {
            tracing::warn!("output device lost");
            self.close_output();
            events.push(EngineEvent::DeviceLost);
        } else if signal.invalidated {
            // A stream is also invalidated when its device is unplugged. Reopening would
            // then land on the default device, i.e. move the music to the speakers.
            let gone = device_id
                .as_deref()
                .is_some_and(|id| output::device_present(id) == Some(false));
            // While we set the device's rate ourselves, the notification of our own
            // change can arrive after the stream was opened at the new rate. A stream
            // whose device runs at the stream's rate has nothing to be rebuilt for, and
            // rebuilding would change the rate again: a loop.
            let stale = !gone
                && self.hold.is_some()
                && device_id.as_deref().and_then(output::device_nominal_rate) == self.sample_rate();
            if stale {
                tracing::debug!("rate notification for the rate already in use; ignored");
                // The stream itself is fine, but the backend stopped it before telling
                // us. Without this the music would stay silent until the stall watchdog.
                if let Sink::Device(stream) = &mut self.sink
                    && let Err(e) = stream.resume_after_notification()
                {
                    tracing::warn!("could not restart the output stream: {e}");
                    self.signals.signal_lost();
                }
            } else {
                tracing::info!(gone, "output stream invalidated");
                if gone {
                    self.close_output();
                    events.push(EngineEvent::DeviceLost);
                } else {
                    // Keep the claim: the player reopens the stream on the same device.
                    self.close_stream();
                    events.push(EngineEvent::OutputChanged);
                }
            }
        } else if self.last_device_poll.elapsed() >= DEVICE_POLL
            && let Some(current_id) = device_id
        {
            // Follow the system default device, but only when the user changed it while the
            // old device is still connected. A vanished device is "lost", never silently
            // replaced. One failed lookup proves nothing (the device list is briefly
            // incomplete while the system reconfigures), so it takes two in a row.
            self.last_device_poll = Instant::now();
            match output::device_present(&current_id) {
                Some(false) => {
                    self.device_missing_polls += 1;
                    if self.device_missing_polls >= DEVICE_MISSING_POLLS {
                        tracing::warn!("output device no longer listed");
                        self.close_output();
                        events.push(EngineEvent::DeviceLost);
                    }
                }
                Some(true) => {
                    self.device_missing_polls = 0;
                    // With exclusive access the system moves its default away from the
                    // device we took; that is our doing, not the user choosing a device.
                    let exclusive = self.hold.as_ref().is_some_and(DeviceHold::is_exclusive);
                    if self.device_pref.is_none()
                        && !exclusive
                        && output::default_output_id().is_some_and(|id| id != current_id)
                    {
                        self.close_output();
                        events.push(EngineEvent::OutputChanged);
                    }
                }
                // The system could not be asked; keep playing and ask again.
                None => {}
            }
        }

        // Stop the device callback after a while of silence.
        if let (Some(since), Sink::Device(stream)) = (self.idle_since, &mut self.sink)
            && stream.is_running()
            && since.elapsed() >= IDLE_STOP
            && self.loading.is_none()
        {
            let _ = stream.set_running(false);
        }
    }
}
