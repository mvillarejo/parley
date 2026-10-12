//! A recording session: the mic and the system audio are written to two WAV
//! files as they arrive, aligned to one recording clock that stops while the
//! recording is paused.

use parley_lib::wav::{silence_to_insert, WavWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: u32 = crate::audio::TARGET_SAMPLE_RATE;
/// A track that falls this far behind the clock is padded with silence.
const ALIGN_TOLERANCE_SAMPLES: u64 = SAMPLE_RATE as u64 / 2;
const HEADER_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const DURABLE_SYNC_INTERVAL: Duration = Duration::from_secs(10);

/// Recording time: runs while recording, stands still while paused, and is
/// frozen for good on stop.
#[derive(Debug, Default)]
pub struct RecordingClock {
    inner: Mutex<ClockState>,
}

#[derive(Debug, Default)]
struct ClockState {
    accumulated: Duration,
    running_since: Option<Instant>,
    frozen: bool,
}

impl RecordingClock {
    pub fn started() -> Self {
        Self::started_at(Instant::now())
    }

    pub fn started_at(now: Instant) -> Self {
        Self {
            inner: Mutex::new(ClockState {
                accumulated: Duration::ZERO,
                running_since: Some(now),
                frozen: false,
            }),
        }
    }

    pub fn elapsed_at(&self, now: Instant) -> Duration {
        let s = self.inner.lock().unwrap();
        s.accumulated + s.running_since.map(|t| now - t).unwrap_or_default()
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now())
    }

    /// Recording time in ms: the clock highlights and notes are stamped with.
    /// The tracks are padded to this same clock, so it lines up with the
    /// transcript's `start_ms`.
    pub fn elapsed_ms_at(&self, now: Instant) -> i64 {
        self.elapsed_at(now).as_millis() as i64
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.elapsed_ms_at(Instant::now())
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().running_since.is_some()
    }

    pub fn pause_at(&self, now: Instant) {
        let mut s = self.inner.lock().unwrap();
        if let Some(t) = s.running_since.take() {
            s.accumulated += now - t;
        }
    }

    pub fn pause(&self) {
        self.pause_at(Instant::now())
    }

    pub fn resume_at(&self, now: Instant) {
        let mut s = self.inner.lock().unwrap();
        if !s.frozen && s.running_since.is_none() {
            s.running_since = Some(now);
        }
    }

    pub fn resume(&self) {
        self.resume_at(Instant::now())
    }

    pub fn freeze(&self) {
        self.pause();
        self.inner.lock().unwrap().frozen = true;
    }

    pub fn elapsed_samples(&self) -> u64 {
        (self.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64
    }
}

/// Meter level in 0..=1 from a chunk: RMS mapped from -60 dBFS..0 dBFS.
pub fn level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let v = s as f64 / i16::MAX as f64;
            v * v
        })
        .sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    (((db + 60.0) / 60.0).clamp(0.0, 1.0)) as f32
}

/// Shared meter value (f32 stored as bits).
#[derive(Debug, Default)]
pub struct Level(AtomicU32);

impl Level {
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Write one track until `chunks` ends, then pad it to the final clock so both
/// tracks have the same length. Returns the number of samples written.
pub fn write_track(
    chunks: impl Iterator<Item = Vec<i16>>,
    path: &Path,
    clock: &RecordingClock,
    meter: &Level,
) -> std::io::Result<u64> {
    let mut writer = WavWriter::create(path, SAMPLE_RATE)?;
    let mut last_sync = Instant::now();
    let mut last_durable = Instant::now();
    for chunk in chunks {
        if !clock.is_running() {
            meter.set(0.0);
            continue;
        }
        let pad = silence_to_insert(
            writer.samples_written(),
            chunk.len() as u64,
            clock.elapsed_samples(),
            ALIGN_TOLERANCE_SAMPLES,
        );
        if pad > 0 {
            writer.write_silence(pad)?;
        }
        writer.write_samples(&chunk)?;
        meter.set(level(&chunk));
        if last_sync.elapsed() >= HEADER_SYNC_INTERVAL {
            let durable = last_durable.elapsed() >= DURABLE_SYNC_INTERVAL;
            writer.sync_header(durable)?;
            last_sync = Instant::now();
            if durable {
                last_durable = Instant::now();
            }
        }
    }
    meter.set(0.0);
    let target = clock.elapsed_samples();
    if target > writer.samples_written() {
        writer.write_silence(target - writer.samples_written())?;
    }
    writer.finalize()
}

pub struct TrackHandle {
    pub path: PathBuf,
    pub meter: Arc<Level>,
    pub thread: JoinHandle<std::io::Result<u64>>,
}

pub fn spawn_track<I>(
    name: &str,
    chunks: I,
    path: PathBuf,
    clock: Arc<RecordingClock>,
) -> std::io::Result<TrackHandle>
where
    I: Iterator<Item = Vec<i16>> + Send + 'static,
{
    let meter = Arc::new(Level::default());
    let thread_meter = meter.clone();
    let thread_path = path.clone();
    let thread = std::thread::Builder::new()
        .name(format!("track-{name}"))
        .spawn(move || write_track(chunks, &thread_path, &clock, &thread_meter))?;
    Ok(TrackHandle {
        path,
        meter,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley_lib::wav::read_wav;

    #[test]
    fn clock_counts_only_running_time() {
        let t0 = Instant::now();
        let clock = RecordingClock::started_at(t0);
        assert_eq!(
            clock.elapsed_at(t0 + Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        clock.pause_at(t0 + Duration::from_secs(5));
        assert!(!clock.is_running());
        assert_eq!(
            clock.elapsed_at(t0 + Duration::from_secs(60)),
            Duration::from_secs(5)
        );
        clock.resume_at(t0 + Duration::from_secs(60));
        assert_eq!(
            clock.elapsed_at(t0 + Duration::from_secs(62)),
            Duration::from_secs(7)
        );
        // Double pause/resume are harmless.
        clock.pause_at(t0 + Duration::from_secs(62));
        clock.pause_at(t0 + Duration::from_secs(70));
        assert_eq!(
            clock.elapsed_at(t0 + Duration::from_secs(80)),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn marker_time_excludes_paused_time() {
        let t0 = Instant::now();
        let clock = RecordingClock::started_at(t0);
        let s = Duration::from_secs;
        assert_eq!(
            clock.elapsed_ms_at(t0 + Duration::from_millis(12_345)),
            12_345
        );
        // Paused at 20 s for 40 s: a mark made while paused is at 20 s…
        clock.pause_at(t0 + s(20));
        assert_eq!(clock.elapsed_ms_at(t0 + s(45)), 20_000);
        clock.resume_at(t0 + s(60));
        // …and 5 s after resuming it is 25 s, not 65 s, matching the audio.
        assert_eq!(clock.elapsed_ms_at(t0 + s(65)), 25_000);
        assert_eq!(
            clock.elapsed_ms_at(t0 + s(65)) as u64 * SAMPLE_RATE as u64 / 1000,
            (clock.elapsed_at(t0 + s(65)).as_secs_f64() * SAMPLE_RATE as f64) as u64
        );
        // A second pause/resume is excluded too.
        clock.pause_at(t0 + s(70));
        clock.resume_at(t0 + s(100));
        assert_eq!(clock.elapsed_ms_at(t0 + s(101)), 31_000);
    }

    #[test]
    fn frozen_clock_never_resumes() {
        let clock = RecordingClock::started();
        clock.freeze();
        clock.resume();
        assert!(!clock.is_running());
    }

    #[test]
    fn level_maps_rms_to_a_meter() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 100]), 0.0);
        assert!((level(&[i16::MAX; 100]) - 1.0).abs() < 1e-3);
        let quiet = level(&[33; 100]); // ~ -60 dBFS
        assert!(quiet < 0.02, "{quiet}");
        let mid = level(&[3277; 100]); // ~ -20 dBFS
        assert!((mid - 0.667).abs() < 0.01, "{mid}");
    }

    #[test]
    fn level_round_trips_through_the_atomic() {
        let l = Level::default();
        assert_eq!(l.get(), 0.0);
        l.set(0.42);
        assert_eq!(l.get(), 0.42);
    }

    #[test]
    fn writer_pads_to_the_frozen_clock_and_drops_paused_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        // A clock that already ran 1 s and is now frozen: every chunk is
        // dropped (as when paused) and the file is padded to exactly 1 s.
        let t0 = Instant::now() - Duration::from_secs(1);
        let clock = RecordingClock::started_at(t0);
        clock.pause_at(t0 + Duration::from_secs(1));
        let meter = Level::default();
        let n = write_track(
            vec![vec![500i16; 1600]; 5].into_iter(),
            &path,
            &clock,
            &meter,
        )
        .unwrap();
        assert_eq!(n, SAMPLE_RATE as u64);
        let wav = read_wav(&path).unwrap();
        assert_eq!(wav.samples.len(), SAMPLE_RATE as usize);
        assert!(wav.samples.iter().all(|&s| s == 0));
        assert_eq!(meter.get(), 0.0);
    }

    #[test]
    fn writer_keeps_running_audio_and_aligns_a_late_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.wav");
        // The clock started 2 s ago; the first chunk only arrives now, so the
        // track gets ~2 s of leading silence before it.
        let clock = RecordingClock::started_at(Instant::now() - Duration::from_secs(2));
        let meter = Level::default();
        let n = write_track(vec![vec![1000i16; 1600]].into_iter(), &path, &clock, &meter).unwrap();
        let wav = read_wav(&path).unwrap();
        assert_eq!(wav.samples.len() as u64, n);
        let first_audio = wav.samples.iter().position(|&s| s != 0).unwrap();
        assert!(
            (30_000..=32_500).contains(&first_audio),
            "audio starts at sample {first_audio}"
        );
        assert!(n >= 32_000, "{n}");
    }

    #[test]
    fn spawned_track_finishes_when_the_source_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<i16>>();
        let clock = Arc::new(RecordingClock::started_at(
            Instant::now() - Duration::from_secs(1),
        ));
        let handle =
            spawn_track("t", rx.into_iter(), dir.path().join("t.wav"), clock.clone()).unwrap();
        tx.send(vec![100; 160]).unwrap();
        drop(tx);
        clock.freeze();
        let n = handle.thread.join().unwrap().unwrap();
        assert!(n >= SAMPLE_RATE as u64);
        assert!(read_wav(&handle.path).is_ok());
    }
}
