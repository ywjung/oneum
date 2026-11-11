//! Tech spike: decode local files with Symphonia and play them on the real output
//! device through CPAL, using the decode-thread → SPSC ring → callback layout.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn decode(path: &str) -> Result<(u32, usize, Vec<f32>), Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format =
        symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let track_id = track.id;
    let (delay, padding, nframes) = (track.delay, track.padding, track.num_frames);
    let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
        return Err("not audio".into());
    };
    let mut decoder =
        symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default().gapless(true))?;
    let mut out = Vec::new();
    let mut tmp = Vec::new();
    let (mut rate, mut ch) = (0, 0);
    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        let buf = decoder.decode(&packet)?;
        rate = buf.spec().rate();
        ch = buf.spec().channels().count();
        buf.copy_to_vec_interleaved::<f32>(&mut tmp);
        out.extend_from_slice(&tmp);
    }
    println!(
        "  decoded {path}: codec={:?} rate={rate} ch={ch} frames={} (container: num_frames={nframes:?} delay={delay:?} padding={padding:?})",
        params.codec,
        out.len() / ch.max(1)
    );
    Ok((rate, ch, out))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let cfg = device.default_output_config()?;
    println!("device: {:?} config: {cfg:?}", device.id());
    let out_rate = cfg.sample_rate();
    let out_ch = cfg.channels() as usize;
    let sample_format = cfg.sample_format();
    for path in std::env::args().skip(1) {
        let (rate, ch, pcm) = decode(&path)?;
        if rate != out_rate {
            println!("  skip playback: file rate {rate} != device rate {out_rate} (resampler not part of this spike)");
            continue;
        }
        let (mut prod, mut cons) = rtrb::RingBuffer::<f32>::new(out_rate as usize * out_ch / 2);
        let played = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        let (p2, u2) = (played.clone(), underruns.clone());
        assert_eq!(sample_format, cpal::SampleFormat::F32, "spike only handles f32 output");
        let stream = device.build_output_stream(
            cfg.clone().into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut n = 0u64;
                for s in data.iter_mut() {
                    match cons.pop() {
                        Ok(v) => {
                            *s = v;
                            n += 1;
                        }
                        Err(_) => *s = 0.0,
                    }
                }
                p2.fetch_add(n, Ordering::Relaxed);
                if (n as usize) < data.len() {
                    u2.fetch_add(1, Ordering::Relaxed);
                }
            },
            |e| eprintln!("stream error: {e}"),
            None,
        )?;
        // Pre-fill, then start and keep feeding from this (non-realtime) thread.
        let total_frames = pcm.len() / ch;
        let mut frame = 0;
        let mut started = false;
        let t0 = std::time::Instant::now();
        while frame < total_frames {
            while frame < total_frames && prod.slots() >= out_ch {
                for c in 0..out_ch {
                    let v = pcm[frame * ch + c.min(ch - 1)];
                    let _ = prod.push(v);
                }
                frame += 1;
            }
            if !started {
                stream.play()?;
                started = true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        while played.load(Ordering::Relaxed) < (total_frames * out_ch) as u64
            && t0.elapsed() < std::time::Duration::from_secs(10)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mid_underruns = underruns.load(Ordering::Relaxed);
        drop(stream);
        println!(
            "  played {} / {} frames in {:.2}s, callbacks short of data: {} (tail drain included)",
            played.load(Ordering::Relaxed) / out_ch as u64,
            total_frames,
            t0.elapsed().as_secs_f64(),
            mid_underruns
        );
    }
    Ok(())
}
