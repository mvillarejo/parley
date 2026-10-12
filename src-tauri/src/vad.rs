//! Energy-based voice activity detection. Splits a track into utterances so
//! each request to a batch provider is short, starts at a known time, and
//! silence is never sent (silence makes Whisper-style models hallucinate).

#[derive(Debug, Clone, PartialEq)]
pub struct VadConfig {
    pub frame_ms: u32,
    /// Speech runs shorter than this are dropped (clicks, keyboard taps).
    pub min_speech_ms: u32,
    /// Runs separated by less silence than this belong to one utterance.
    pub merge_gap_ms: u32,
    /// Context kept before and after each utterance.
    pub pad_ms: u32,
    /// Hard cap per utterance (provider limit); longer speech is split at the
    /// quietest frame in the second half of the window.
    pub max_utterance_ms: u32,
    /// Absolute RMS floor (0..1) below which a frame is never speech.
    pub min_threshold: f32,
    /// Speech must be this many times louder than the track's noise floor.
    pub noise_factor: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_ms: 30,
            min_speech_ms: 300,
            merge_gap_ms: 900,
            pad_ms: 200,
            max_utterance_ms: 28_000,
            min_threshold: 0.008,
            noise_factor: 3.0,
        }
    }
}

/// Sample range `[start, end)` of one utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
}

impl Region {
    pub fn start_ms(&self, rate: u32) -> u64 {
        self.start as u64 * 1000 / rate.max(1) as u64
    }
    pub fn end_ms(&self, rate: u32) -> u64 {
        self.end as u64 * 1000 / rate.max(1) as u64
    }
}

fn frame_rms(samples: &[i16], frame: usize) -> Vec<f32> {
    samples
        .chunks(frame)
        .map(|chunk| {
            let sum: f64 = chunk
                .iter()
                .map(|&s| {
                    let v = s as f64 / i16::MAX as f64;
                    v * v
                })
                .sum();
            (sum / chunk.len() as f64).sqrt() as f32
        })
        .collect()
}

fn threshold(rms: &[f32], cfg: &VadConfig) -> f32 {
    let mut sorted: Vec<f32> = rms.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pick = |q: usize| {
        sorted
            .get(sorted.len() * q / 100)
            .copied()
            .unwrap_or_default()
    };
    // Noise floor from the quiet end, but never above half the loud end, so a
    // track that is almost all speech (low percentile = speech) still works.
    let noise = pick(20) * cfg.noise_factor;
    let loud = pick(90) * 0.5;
    noise.min(loud).max(cfg.min_threshold)
}

pub fn speech_regions(samples: &[i16], rate: u32, cfg: &VadConfig) -> Vec<Region> {
    let frame = ((rate as u64 * cfg.frame_ms as u64) / 1000).max(1) as usize;
    if samples.is_empty() {
        return Vec::new();
    }
    let rms = frame_rms(samples, frame);
    let thr = threshold(&rms, cfg);
    let ms_to_frames = |ms: u32| (ms as usize).div_ceil(cfg.frame_ms.max(1) as usize);
    let merge_gap = ms_to_frames(cfg.merge_gap_ms);
    let min_speech = ms_to_frames(cfg.min_speech_ms);
    let pad = ms_to_frames(cfg.pad_ms);
    let max_frames = (cfg.max_utterance_ms / cfg.frame_ms.max(1)).max(2) as usize;

    // Voiced runs in frames, merged across short pauses.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < rms.len() {
        if rms[i] >= thr {
            let start = i;
            while i < rms.len() && rms[i] >= thr {
                i += 1;
            }
            match runs.last_mut() {
                Some(last) if start - last.1 <= merge_gap => last.1 = i,
                _ => runs.push((start, i)),
            }
        } else {
            i += 1;
        }
    }
    runs.retain(|(s, e)| e - s >= min_speech);

    // Pad, merging runs whose padding overlaps.
    let mut padded: Vec<(usize, usize)> = Vec::new();
    for (s, e) in runs {
        let s = s.saturating_sub(pad);
        let e = (e + pad).min(rms.len());
        match padded.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => padded.push((s, e)),
        }
    }

    // Enforce the per-utterance cap, cutting at the quietest frame.
    let mut out = Vec::new();
    for (mut s, e) in padded {
        while e - s > max_frames {
            let lo = s + max_frames / 2;
            let hi = s + max_frames;
            let cut = (lo..hi)
                .min_by(|a, b| rms[*a].total_cmp(&rms[*b]))
                .unwrap_or(hi);
            out.push((s, cut));
            s = cut;
        }
        out.push((s, e));
    }

    out.into_iter()
        .map(|(s, e)| Region {
            start: (s * frame).min(samples.len()),
            end: (e * frame).min(samples.len()),
        })
        .filter(|r| r.end > r.start)
        .collect()
}

/// Whether a track holds any speech at all (skip silent tracks entirely).
pub fn has_speech(samples: &[i16], rate: u32, cfg: &VadConfig) -> bool {
    !speech_regions(samples, rate, cfg).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(ms: u32, amp: f32) -> Vec<i16> {
        let n = (RATE * ms / 1000) as usize;
        (0..n)
            .map(|i| ((i as f32 * 0.2).sin() * amp * i16::MAX as f32) as i16)
            .collect()
    }

    fn silence(ms: u32) -> Vec<i16> {
        vec![0; (RATE * ms / 1000) as usize]
    }

    fn concat(parts: &[Vec<i16>]) -> Vec<i16> {
        parts.concat()
    }

    #[test]
    fn empty_and_silent_tracks_have_no_regions() {
        let cfg = VadConfig::default();
        assert!(speech_regions(&[], RATE, &cfg).is_empty());
        assert!(speech_regions(&silence(5000), RATE, &cfg).is_empty());
        assert!(!has_speech(&silence(1000), RATE, &cfg));
    }

    #[test]
    fn finds_two_utterances_separated_by_a_long_pause() {
        let cfg = VadConfig::default();
        let audio = concat(&[
            silence(1000),
            tone(2000, 0.3),
            silence(3000),
            tone(1500, 0.3),
            silence(500),
        ]);
        let regions = speech_regions(&audio, RATE, &cfg);
        assert_eq!(regions.len(), 2, "{regions:?}");
        // Padded by ~200 ms around the speech.
        let first = regions[0];
        assert!((780..=820).contains(&first.start_ms(RATE)), "{first:?}");
        assert!((3180..=3240).contains(&first.end_ms(RATE)), "{first:?}");
        let second = regions[1];
        assert!((5780..=5820).contains(&second.start_ms(RATE)), "{second:?}");
    }

    #[test]
    fn short_pauses_stay_in_one_utterance() {
        let cfg = VadConfig::default();
        let audio = concat(&[tone(1000, 0.3), silence(400), tone(1000, 0.3)]);
        assert_eq!(speech_regions(&audio, RATE, &cfg).len(), 1);
    }

    #[test]
    fn clicks_shorter_than_min_speech_are_dropped() {
        let cfg = VadConfig::default();
        let audio = concat(&[silence(1000), tone(60, 0.8), silence(1000)]);
        assert!(speech_regions(&audio, RATE, &cfg).is_empty());
    }

    #[test]
    fn steady_background_noise_raises_the_threshold() {
        let cfg = VadConfig::default();
        // Constant hum at ~0.02 RMS with speech 10x louder on top.
        let mut audio = tone(6000, 0.03);
        let speech = tone(1000, 0.3);
        for (i, s) in speech.iter().enumerate() {
            audio[16_000 * 2 + i] = *s;
        }
        let regions = speech_regions(&audio, RATE, &cfg);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert!(regions[0].end_ms(RATE) - regions[0].start_ms(RATE) < 1600);
    }

    #[test]
    fn long_speech_is_split_under_the_cap() {
        let cfg = VadConfig {
            max_utterance_ms: 10_000,
            ..VadConfig::default()
        };
        let audio = tone(35_000, 0.3);
        let regions = speech_regions(&audio, RATE, &cfg);
        assert!(regions.len() >= 4, "{regions:?}");
        for r in &regions {
            assert!(r.end_ms(RATE) - r.start_ms(RATE) <= 10_000, "{r:?}");
        }
        // Contiguous cover of the whole track.
        assert_eq!(regions[0].start, 0);
        assert_eq!(regions.last().unwrap().end, audio.len());
        for pair in regions.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
    }
}
