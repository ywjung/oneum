//! Ties feeds, the renderer and the output device together. Owned by the player thread.
use super::PlaybackError;
use super::decoder::SourceInfo;
use super::dsp::{ReplayGainSettings, volume_to_gain};
use super::feed::{FeedEvent, FeedHandle, FeedParams, NextSlot, RgShared, TrackRequest};
use super::output::{self, OutputError, OutputEvent, OutputStream};
use super::render::{RenderLink, RenderShared, Renderer, render_pair};
use crate::model::TrackId;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Audio that must be buffered before a new feed is handed to the callback.
const PREBUFFER_SECONDS: f64 = 0.12;
/// A silent stream is stopped after this long so the device and the Mac can sleep.
const IDLE_STOP: Duration = Duration::from_secs(3);
const DEVICE_POLL: Duration = Duration::from_secs(1);

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
    sink: Sink,
    /// `None` = follow the system default device.
    device_pref: Option<String>,
    feed_tx: Sender<FeedEvent>,
    feed_rx: Receiver<FeedEvent>,
    out_tx: Sender<OutputEvent>,
    out_rx: Receiver<OutputEvent>,
    /// Feed whose consumer has been handed to the renderer.
    active: Option<FeedHandle>,
    /// Feed still pre-buffering.
    loading: Option<FeedHandle>,
    last_epoch: u16,
    reported_drain: u16,
    idle_since: Option<Instant>,
    last_device_poll: Instant,
}

impl Engine {
    pub fn new(device_pref: Option<String>, volume: f32, rg: ReplayGainSettings) -> Self {
        let (feed_tx, feed_rx) = channel();
        let (out_tx, out_rx) = channel();
        Self {
            shared: RenderShared::new(volume_to_gain(volume)),
            rg: RgShared::new(rg),
            sink: Sink::Closed,
            device_pref,
            feed_tx,
            feed_rx,
            out_tx,
            out_rx,
            active: None,
            loading: None,
            last_epoch: 0,
            reported_drain: 0,
            idle_since: None,
            last_device_poll: Instant::now(),
        }
    }

    /// Engine without a device: the returned renderer stands in for the output callback.
    pub fn new_offline(sample_rate: u32, volume: f32, rg: ReplayGainSettings) -> (Self, Renderer) {
        let mut engine = Self::new(None, volume, rg);
        let (renderer, link) = render_pair(engine.shared.clone(), sample_rate);
        engine.sink = Sink::Offline(link);
        (engine, renderer)
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

    /// Opens the output stream if it is not open. Existing feeds are dropped because the
    /// new stream may run at a different sample rate.
    pub fn open_output(&mut self) -> Result<(), OutputError> {
        if self.is_output_open() {
            return Ok(());
        }
        self.stop_feeds();
        let stream = OutputStream::open(
            self.device_pref.as_deref(),
            self.shared.clone(),
            self.out_tx.clone(),
        )?;
        tracing::info!(
            rate = stream.sample_rate(),
            channels = stream.channels,
            "output stream opened"
        );
        self.sink = Sink::Device(Box::new(stream));
        self.idle_since = Some(Instant::now());
        Ok(())
    }

    pub fn close_output(&mut self) {
        self.stop_feeds();
        if matches!(self.sink, Sink::Device(_)) {
            self.sink = Sink::Closed;
        }
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
        };
        let handle = FeedHandle::spawn(params)
            .ok_or_else(|| OutputError::Backend("cannot spawn decode thread".into()))?;
        self.loading = Some(handle); // replaces (and cancels) a feed that was still loading
        // Silence whatever is playing right now; only the new feed may be heard.
        self.shared.require_epoch(epoch);
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
            let _ = self.out_tx.send(OutputEvent::DeviceLost);
        }
    }

    /// `volume` is the 0..=1 slider value.
    pub fn set_volume(&self, volume: f32) {
        self.shared.set_volume_gain(volume_to_gain(volume));
    }

    pub fn set_replaygain(&self, settings: ReplayGainSettings) {
        self.rg.set(settings);
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

    pub fn underruns(&self) -> u64 {
        self.shared.underruns()
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
                } if current(epoch) => {
                    events.push(EngineEvent::TrackStarted {
                        epoch,
                        item_id,
                        track_id,
                        at_frame,
                        start_ms,
                        info,
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
            events.push(EngineEvent::Drained { epoch: drained });
        }

        while let Ok(event) = self.out_rx.try_recv() {
            match event {
                OutputEvent::DeviceLost => {
                    tracing::warn!("output device lost");
                    self.close_output();
                    events.push(EngineEvent::DeviceLost);
                }
                OutputEvent::Invalidated => {
                    tracing::info!("output stream invalidated");
                    self.close_output();
                    events.push(EngineEvent::OutputChanged);
                }
                OutputEvent::Notice(msg) => tracing::debug!("output notice: {msg}"),
            }
        }

        // Follow the system default device, but only when the user changed it while the
        // old device is still connected. A vanished device is "lost", never silently replaced.
        if self.last_device_poll.elapsed() >= DEVICE_POLL {
            self.last_device_poll = Instant::now();
            if let Sink::Device(stream) = &self.sink {
                let current_id = stream.device_id.clone();
                if !output::device_exists(&current_id) {
                    self.close_output();
                    events.push(EngineEvent::DeviceLost);
                } else if self.device_pref.is_none()
                    && output::default_output_id().is_some_and(|id| id != current_id)
                {
                    self.close_output();
                    events.push(EngineEvent::OutputChanged);
                }
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
