//! Delivery metrics measured from raw microphone PCM rather than from the
//! transcript, so they are identical no matter which transcription engine ran.
//!
//! The recording MUST be captured with `VadPolicy::Disabled`. The dictation VAD
//! drops non-speech frames before they reach the buffer, which deletes every
//! pause this module exists to measure.
//!
//! Timing vocabulary, because three different "durations" matter and mixing them
//! up produces nonsense pace numbers:
//! - `duration_seconds` — the whole recording, including dead air at either end.
//! - `speech_span_seconds` — first speech to last speech. Pace is measured over
//!   this, so hitting record and pausing to collect yourself does not count
//!   against WPM.
//! - `speaking_seconds` — time actually above the silence threshold. Articulation
//!   rate is measured over this, which is what separates "talks fast" from
//!   "never pauses".
//!
//! ponytail: energy-threshold VAD + autocorrelation F0 with parabolic peak
//! interpolation. Upgrade to an ML VAD / YIN refinement if noisy-room accuracy
//! complains. Word-aligned metrics (rolling WPM, filler locations) need token
//! timings from the transcription engine.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, HashSet};

// Calibration knobs. Real rooms are not the ideal case these defaults assume,
// so leave them reachable rather than inlining the numbers.
const FRAME_SECONDS: f64 = 0.020;
const PITCH_WINDOW_SECONDS: f64 = 0.050;
const MIN_PAUSE_SECONDS: f64 = 0.5;
/// Pauses this long read as hesitation rather than emphasis.
const LONG_PAUSE_SECONDS: f64 = 2.0;

const ABSOLUTE_SILENCE_FLOOR_DB: f64 = -50.0;
/// How far above the estimated room noise floor counts as speech.
const SILENCE_MARGIN_DB: f64 = 10.0;

const MIN_PITCH_HZ: f64 = 60.0;
/// Above the top of the human speaking range on purpose: capping at ~400 Hz
/// makes high voices wrap to a half-frequency octave error.
const MAX_PITCH_HZ: f64 = 500.0;
/// Normalized autocorrelation peak required to call a frame voiced.
const VOICING_THRESHOLD: f64 = 0.3;
/// How close to the best correlation an earlier peak must be to be preferred
/// over it. Guards against reporting half or a third of the true pitch.
const SUBHARMONIC_TOLERANCE: f64 = 0.9;

const MAX_CONTOUR_POINTS: usize = 240;

/// Minimum separation between the loud and quiet populations for the delivery
/// numbers to be trustworthy. Speech in a quiet room clears 30 dB and in a noisy
/// one still clears ~18 dB; a recording of room tone alone sits under 8 dB.
const MIN_SIGNAL_TO_NOISE_DB: f64 = 12.0;
/// Below this the recording is simply too faint to analyze, however clean it is.
const MIN_SPEECH_LEVEL_DB: f64 = -45.0;
/// Above this fraction of samples pinned at full scale, the waveform is distorted
/// and every level-derived number is compressed.
const MAX_CLIPPED_SAMPLE_RATIO: f64 = 0.005;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Pause {
    pub start_seconds: f64,
    pub duration_seconds: f64,
}

/// Whether the recording is good enough for the delivery numbers to mean anything.
///
/// Every other metric is computed against an *adaptive* threshold, which is what
/// makes them robust across rooms and microphones — and also what makes them fail
/// silently. A recording of nothing but room tone has its threshold fitted to the
/// room tone, so it reads as continuous confident speech. The numbers stay precise
/// while becoming entirely fictional, which is worse than refusing to answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct SignalQuality {
    /// 95th minus 10th percentile of frame levels. Speech typically clears 20 dB.
    pub signal_to_noise_db: f64,
    pub speech_level_db: f64,
    pub clipped_sample_ratio: f64,
    pub is_reliable: bool,
    /// Why the recording was rejected, phrased for the user. None when reliable.
    pub warning: Option<String>,
}

impl Default for SignalQuality {
    fn default() -> Self {
        Self {
            signal_to_noise_db: 0.0,
            speech_level_db: 0.0,
            clipped_sample_ratio: 0.0,
            is_reliable: false,
            warning: Some("No audio was analyzed.".to_string()),
        }
    }
}

/// Stored as JSON alongside history entries. Any field added later needs
/// `#[serde(default)]`, or sessions saved before it existed stop decoding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct DeliveryMetrics {
    pub duration_seconds: f64,
    pub speech_span_seconds: f64,
    pub speaking_seconds: f64,
    pub total_words: u32,

    /// Silence runs inside the speech span only.
    pub pauses: Vec<Pause>,
    pub total_pause_seconds: f64,
    pub longest_pause_seconds: f64,
    pub speaking_ratio: f64,

    pub words_per_minute: f64,
    /// WPM excluding pauses.
    pub articulation_rate: f64,

    pub filler_counts: BTreeMap<String, u32>,
    pub fillers_per_minute: f64,

    pub mean_pitch_hz: f64,
    pub pitch_range_hz: f64,
    pub pitch_std_dev_hz: f64,
    /// 0 means unvoiced.
    pub pitch_contour: Vec<f64>,

    /// dB relative to full scale, so values are negative.
    pub mean_level_db: f64,
    pub dynamic_range_db: f64,
    pub level_std_dev_db: f64,
    pub energy_contour: Vec<f64>,

    /// Spacing between contour samples; both contours share this grid.
    pub contour_interval_seconds: f64,

    /// Read this before trusting anything above it.
    #[serde(default)]
    pub quality: SignalQuality,
}

impl DeliveryMetrics {
    pub fn total_fillers(&self) -> u32 {
        self.filler_counts.values().sum()
    }

    pub fn long_pauses(&self) -> Vec<&Pause> {
        self.pauses
            .iter()
            .filter(|p| p.duration_seconds >= LONG_PAUSE_SECONDS)
            .collect()
    }

    /// Compact rendering handed to the coaching LLM alongside the transcript.
    /// Units are spelled out because the model has to cite these numbers back.
    pub fn summary_text(&self) -> String {
        // Withhold the numbers rather than caveating them. A model handed unreliable
        // measurements plus a warning will still reason about them — we watched it
        // explain, fluently and precisely, a 24.5 WPM delivery that never happened.
        if !self.quality.is_reliable {
            let reason = self
                .quality
                .warning
                .as_deref()
                .unwrap_or("unknown audio problem");
            return format!(
                "DELIVERY METRICS UNAVAILABLE\n\
                 The recording could not be measured reliably: {reason}.\n\
                 Do not comment on pace, pauses, pitch, volume or filler rate, and do not \
                 estimate them from the transcript. Say plainly that delivery could not be \
                 measured this time and that the recording needs to be redone, then critique \
                 the content only."
            );
        }

        let mut lines = vec![
            "MEASURED DELIVERY METRICS".to_string(),
            format!(
                "Duration: {:.1}s total, {:.1}s from first to last word",
                self.duration_seconds, self.speech_span_seconds
            ),
            format!("Words: {}", self.total_words),
            format!("Speaking rate: {:.1} WPM overall", self.words_per_minute),
            format!(
                "Articulation rate: {:.1} WPM excluding pauses",
                self.articulation_rate
            ),
            format!(
                "Speaking vs silence: {:.1}% of the speech span was voiced",
                self.speaking_ratio * 100.0
            ),
            format!(
                "Pauses (>= 0.5s): {}, totalling {:.1}s",
                self.pauses.len(),
                self.total_pause_seconds
            ),
            format!("Longest pause: {:.1}s", self.longest_pause_seconds),
        ];

        let long = self.long_pauses();
        if long.is_empty() {
            lines.push("Pauses >= 2s: none".to_string());
        } else {
            let stamps: Vec<String> = long
                .iter()
                .map(|p| format!("{:.1}s for {:.1}s", p.start_seconds, p.duration_seconds))
                .collect();
            lines.push(format!(
                "Pauses >= 2s: {} ({})",
                long.len(),
                stamps.join(", ")
            ));
        }

        if self.filler_counts.is_empty() {
            lines.push("Filler words: none detected".to_string());
        } else {
            let mut sorted: Vec<_> = self.filler_counts.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let breakdown: Vec<String> = sorted.iter().map(|(w, n)| format!("{w} x{n}")).collect();
            lines.push(format!(
                "Filler words: {} total ({}), {:.1} per minute",
                self.total_fillers(),
                breakdown.join(", "),
                self.fillers_per_minute
            ));
        }

        if self.mean_pitch_hz > 0.0 {
            lines.push(format!(
                "Pitch: mean {:.1} Hz, range {:.1} Hz, standard deviation {:.1} Hz (lower = more monotone)",
                self.mean_pitch_hz, self.pitch_range_hz, self.pitch_std_dev_hz
            ));
        } else {
            lines.push("Pitch: no voiced audio detected".to_string());
        }

        lines.push(format!(
            "Volume: mean {:.1} dBFS, dynamic range {:.1} dB, standard deviation {:.1} dB (lower = flatter delivery)",
            self.mean_level_db, self.dynamic_range_db, self.level_std_dev_db
        ));

        lines.join("\n")
    }
}

/// Pure and synchronous: no state, no settings reads, so it is unit-tested on
/// synthetic PCM. Run it off the UI thread — a 10 minute speech is ~9.6 M samples.
pub fn analyze(
    pcm: &[f32],
    sample_rate: f64,
    raw_transcript: &str,
    filler_words: &[String],
) -> DeliveryMetrics {
    if pcm.is_empty() || sample_rate <= 0.0 {
        return DeliveryMetrics::default();
    }

    let duration = pcm.len() as f64 / sample_rate;
    let frame_length = ((sample_rate * FRAME_SECONDS) as usize).max(1);
    let frame_duration = frame_length as f64 / sample_rate;

    let levels_db = frame_levels_db(pcm, frame_length);
    if levels_db.is_empty() {
        return DeliveryMetrics::default();
    }

    let quality = assess_quality(pcm, &levels_db);

    let threshold = silence_threshold_db(&levels_db);
    let is_speech: Vec<bool> = levels_db.iter().map(|&l| l > threshold).collect();

    // Trim dead air at the ends: waiting to start is not a rhetorical pause.
    let (Some(first), Some(last)) = (
        is_speech.iter().position(|&s| s),
        is_speech.iter().rposition(|&s| s),
    ) else {
        // Nothing above the threshold. Carry the quality verdict out anyway, so the
        // user is told *why* rather than just "no audio".
        return DeliveryMetrics {
            quality,
            ..Default::default()
        };
    };

    let speech_span_seconds = (last - first + 1) as f64 * frame_duration;
    let speaking_frames = is_speech[first..=last].iter().filter(|&&s| s).count();
    let speaking_seconds = speaking_frames as f64 * frame_duration;

    let pauses = detect_pauses(&is_speech, first, last, frame_duration);
    // `+ 0.0`: an empty float sum is -0.0 in Rust, which renders as "-0.0s".
    let total_pause_seconds = pauses.iter().map(|p| p.duration_seconds).sum::<f64>() + 0.0;
    let longest_pause_seconds = pauses
        .iter()
        .map(|p| p.duration_seconds)
        .fold(0.0, f64::max);

    let words: Vec<&str> = raw_transcript.split_whitespace().collect();
    let filler_counts = count_fillers(&words, filler_words);
    let total_fillers: u32 = filler_counts.values().sum();

    let span_minutes = speech_span_seconds / 60.0;
    let speaking_minutes = speaking_seconds / 60.0;
    let per = |count: f64, minutes: f64| if minutes > 0.0 { count / minutes } else { 0.0 };

    let pitches = pitch_track(pcm, sample_rate, threshold);
    let voiced: Vec<f64> = pitches.iter().copied().filter(|&p| p > 0.0).collect();
    // Percentile spread, same reasoning as dynamic range: one octave-error window
    // (a breathy onset, a consonant) should not define someone's pitch range.
    let pitch_range_hz = percentile_spread(&voiced);
    // Mean and deviation use the same 5th–95th band. Autocorrelation's known
    // failure is the octave error, and a handful of windows at 2x the true pitch
    // doubled the measured deviation of a real 110 Hz voice.
    let voiced = trim_to_percentiles(voiced);

    let speaking_levels: Vec<f64> = levels_db
        .iter()
        .zip(&is_speech)
        .filter(|(_, &s)| s)
        .map(|(&l, _)| l)
        .collect();

    // One time grid for both contours. Energy frames (20 ms) and pitch windows
    // (50 ms) have different native rates; publishing them at different lengths
    // against the single contour interval plots short recordings' pitch lines at
    // the wrong time scale, out from under their own pause bands.
    let energy_contour = downsample(&levels_db);
    let pitch_contour = resample_pitch(&pitches, energy_contour.len());

    DeliveryMetrics {
        duration_seconds: duration,
        speech_span_seconds,
        speaking_seconds,
        total_words: words.len() as u32,
        pauses,
        total_pause_seconds,
        longest_pause_seconds,
        speaking_ratio: if speech_span_seconds > 0.0 {
            speaking_seconds / speech_span_seconds
        } else {
            0.0
        },
        words_per_minute: per(words.len() as f64, span_minutes),
        articulation_rate: per(words.len() as f64, speaking_minutes),
        filler_counts,
        fillers_per_minute: per(total_fillers as f64, span_minutes),
        mean_pitch_hz: mean(&voiced),
        pitch_range_hz,
        pitch_std_dev_hz: standard_deviation(&voiced),
        pitch_contour,
        mean_level_db: mean(&speaking_levels),
        dynamic_range_db: percentile_spread(&speaking_levels),
        level_std_dev_db: standard_deviation(&speaking_levels),
        energy_contour,
        contour_interval_seconds: contour_interval(levels_db.len(), frame_duration),
        quality,
    }
}

// --- Signal quality ---

fn assess_quality(pcm: &[f32], levels_db: &[f64]) -> SignalQuality {
    let sorted = sorted(levels_db);
    let noise_floor = percentile(&sorted, 0.10);
    let speech_level = percentile(&sorted, 0.95);
    let separation = speech_level - noise_floor;

    let clipped = pcm.iter().filter(|s| s.abs() >= 0.99).count();
    let clipped_ratio = clipped as f64 / pcm.len() as f64;

    // Most specific failure first: a clipped recording is also loud, and a silent
    // one also has poor separation, so reporting the wrong cause would send the
    // user to fix the wrong thing.
    let warning = if clipped_ratio > MAX_CLIPPED_SAMPLE_RATIO {
        Some(
            "the input is clipping — lower the microphone gain or move further from it".to_string(),
        )
    } else if speech_level < MIN_SPEECH_LEVEL_DB {
        Some(
            "the recording is too quiet — move closer to the microphone or raise its input level"
                .to_string(),
        )
    } else if separation < MIN_SIGNAL_TO_NOISE_DB {
        Some(format!(
            "speech is not clearly separated from background noise ({separation:.0} dB of separation, {MIN_SIGNAL_TO_NOISE_DB:.0} dB needed)"
        ))
    } else {
        None
    };

    SignalQuality {
        signal_to_noise_db: separation,
        speech_level_db: speech_level,
        clipped_sample_ratio: clipped_ratio,
        is_reliable: warning.is_none(),
        warning,
    }
}

// --- Energy ---

/// Per-frame RMS converted to dBFS.
fn frame_levels_db(pcm: &[f32], frame_length: usize) -> Vec<f64> {
    pcm.chunks_exact(frame_length)
        .map(|frame| {
            let sum: f64 = frame.iter().map(|&s| s as f64 * s as f64).sum();
            let rms = (sum / frame_length as f64).sqrt();
            20.0 * rms.max(1e-10).log10()
        })
        .collect()
}

/// Silence threshold bracketed from both ends of the level distribution.
///
/// Estimating it from the noise floor alone breaks on continuous speech: with no
/// silence in the recording the quiet percentile *is* speech, so the margin pushes
/// the threshold above every frame and the whole take reads as silent. Holding it
/// below the loud percentile as well keeps the threshold between the two
/// populations whether or not the recording actually contains silence.
fn silence_threshold_db(levels_db: &[f64]) -> f64 {
    let sorted = sorted(levels_db);
    let noise_floor = percentile(&sorted, 0.10);
    let speech_level = percentile(&sorted, 0.95);
    (noise_floor + SILENCE_MARGIN_DB)
        .min(speech_level - SILENCE_MARGIN_DB)
        .max(ABSOLUTE_SILENCE_FLOOR_DB)
}

fn detect_pauses(is_speech: &[bool], first: usize, last: usize, frame_duration: f64) -> Vec<Pause> {
    let mut pauses = Vec::new();
    let mut run_start: Option<usize> = None;

    for (index, &speech) in is_speech.iter().enumerate().take(last + 1).skip(first) {
        if speech {
            if let Some(start) = run_start.take() {
                let duration = (index - start) as f64 * frame_duration;
                if duration >= MIN_PAUSE_SECONDS {
                    pauses.push(Pause {
                        start_seconds: start as f64 * frame_duration,
                        duration_seconds: duration,
                    });
                }
            }
        } else if run_start.is_none() {
            run_start = Some(index);
        }
    }
    // A trailing run cannot exist: `last` is a speech frame by construction.
    pauses
}

// --- Pitch ---

/// Autocorrelation F0 per non-overlapping window. Unvoiced windows yield 0 so the
/// contour stays aligned to a uniform time axis.
///
/// Windows at or below the silence threshold are unvoiced by definition. Without
/// that gate, room tone gets a pitch: it is low-passed, so neighbouring samples
/// correlate, and the pauses between phrases "measure" as 300–500 Hz.
fn pitch_track(pcm: &[f32], sample_rate: f64, silence_threshold_db: f64) -> Vec<f64> {
    let window_length = ((sample_rate * PITCH_WINDOW_SECONDS) as usize).max(1);
    frame_levels_db(pcm, window_length)
        .into_iter()
        .zip(pcm.chunks_exact(window_length))
        .map(|(level, window)| {
            if level > silence_threshold_db {
                estimate_f0(window, sample_rate).unwrap_or(0.0)
            } else {
                0.0
            }
        })
        .collect()
}

fn estimate_f0(window: &[f32], sample_rate: f64) -> Option<f64> {
    let count = window.len();
    let min_lag = ((sample_rate / MAX_PITCH_HZ) as usize).max(1);
    let max_lag = ((sample_rate / MIN_PITCH_HZ) as usize).min(count.saturating_sub(1));
    if max_lag <= min_lag {
        return None;
    }

    // Prefix sums of squares make each lag's two window energies O(1), which is
    // what keeps proper normalization affordable across every lag.
    let mut prefix = vec![0.0f64; count + 1];
    for (i, &s) in window.iter().enumerate() {
        prefix[i + 1] = prefix[i] + s as f64 * s as f64;
    }
    if prefix[count] <= 0.0 {
        return None;
    }

    // Normalized cross-correlation, so values land in [-1, 1] and are directly
    // comparable across lags. Raw or length-normalized autocorrelation both carry
    // a lag-dependent bias, and for a periodic signal that bias is enough to make
    // a period multiple outscore the true period — an octave error.
    //
    // ponytail: scalar dot product per lag (~4e9 multiply-adds for a 20 minute
    // take, a few seconds in release). FFT-based autocorrelation via the rustfft
    // dependency if that ever shows up in a profile.
    let mut correlations = vec![0.0f64; max_lag + 2];
    for lag in min_lag..=max_lag {
        let dot: f32 = window[..count - lag]
            .iter()
            .zip(&window[lag..])
            .map(|(a, b)| a * b)
            .sum();
        let scale = (prefix[count - lag] * (prefix[count] - prefix[lag])).sqrt();
        correlations[lag] = if scale > 0.0 { dot as f64 / scale } else { 0.0 };
    }

    let peak = correlations[min_lag..=max_lag]
        .iter()
        .copied()
        .fold(f64::MIN, f64::max);
    if peak < VOICING_THRESHOLD {
        return None;
    }

    // Every integer multiple of the true period correlates about as well as the
    // period itself, so the global maximum is not reliably the fundamental. Take
    // the earliest local maximum that is competitive with the global peak;
    // requiring it to be near the peak rejects spurious formant-driven bumps.
    let best_lag = (min_lag..=max_lag)
        .find(|&lag| {
            correlations[lag] >= SUBHARMONIC_TOLERANCE * peak
                && correlations[lag] >= correlations[lag - 1]
                && correlations[lag] >= correlations[lag + 1]
        })
        .unwrap_or(min_lag);

    Some(sample_rate / refine_lag(best_lag, &correlations, max_lag))
}

/// Parabolic interpolation across the peak and its neighbours. Integer lags
/// quantize badly up high — at 16 kHz the lags for 430 Hz and 444 Hz are adjacent,
/// so without this the error near the top of the range is ~15 Hz.
fn refine_lag(lag: usize, correlations: &[f64], max_lag: usize) -> f64 {
    if lag == 0 || lag >= max_lag {
        return lag as f64;
    }
    let (previous, current, next) = (
        correlations[lag - 1],
        correlations[lag],
        correlations[lag + 1],
    );
    let denominator = previous - 2.0 * current + next;
    if denominator.abs() <= f64::EPSILON {
        return lag as f64;
    }
    let offset = 0.5 * (previous - next) / denominator;
    if offset.abs() > 1.0 {
        return lag as f64;
    }
    lag as f64 + offset
}

// --- Transcript ---

/// Matches case-insensitively with surrounding punctuation trimmed.
///
/// ponytail: single tokens only. Multi-word fillers ("you know", "sort of") need
/// n-gram matching.
fn count_fillers(words: &[&str], filler_words: &[String]) -> BTreeMap<String, u32> {
    let fillers: HashSet<String> = filler_words.iter().map(|w| w.to_lowercase()).collect();
    let mut counts = BTreeMap::new();
    if fillers.is_empty() {
        return counts;
    }
    for word in words {
        let normalized = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if fillers.contains(&normalized) {
            *counts.entry(normalized).or_insert(0) += 1;
        }
    }
    counts
}

// --- Statistics ---

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v
}

/// Nearest-rank percentile over an already-sorted slice.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn standard_deviation(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let average = mean(values);
    let variance = values.iter().map(|v| (v - average).powi(2)).sum::<f64>() / values.len() as f64;
    variance.sqrt()
}

/// Keeps the values inside the 5th–95th percentile band.
fn trim_to_percentiles(values: Vec<f64>) -> Vec<f64> {
    let ordered = sorted(&values);
    let (low, high) = (percentile(&ordered, 0.05), percentile(&ordered, 0.95));
    values
        .into_iter()
        .filter(|v| (low..=high).contains(v))
        .collect()
}

/// 5th-to-95th percentile rather than min-to-max: one cough or one clipped
/// syllable should not define someone's "dynamic range".
fn percentile_spread(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let sorted = sorted(values);
    percentile(&sorted, 0.95) - percentile(&sorted, 0.05)
}

/// `count` half-open index ranges that tile `len` items.
fn buckets(len: usize, count: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
    let size = len as f64 / count as f64;
    (0..count).map(move |i| {
        let start = (i as f64 * size) as usize;
        let end = (((i + 1) as f64 * size) as usize).max(start + 1).min(len);
        start..end
    })
}

/// Bucket-average down to a plottable number of points. Energy only — the pitch
/// track goes through `resample_pitch`, because plain averaging would mix unvoiced
/// zeros into voiced buckets.
fn downsample(values: &[f64]) -> Vec<f64> {
    if values.len() <= MAX_CONTOUR_POINTS {
        return values.to_vec();
    }
    buckets(values.len(), MAX_CONTOUR_POINTS)
        .map(|r| mean(&values[r]))
        .collect()
}

/// Puts the pitch track on the energy contour's time grid. Buckets average voiced
/// windows only: averaging a 200 Hz window with an unvoiced zero yields 100 Hz — a
/// pitch that was never spoken — and drags every phrase boundary toward zero,
/// charting the speaker as more monotone than they are. A bucket with no voiced
/// windows stays 0.
fn resample_pitch(pitches: &[f64], count: usize) -> Vec<f64> {
    if pitches.is_empty() || count == 0 {
        return Vec::new();
    }
    if pitches.len() == count {
        return pitches.to_vec();
    }
    buckets(pitches.len(), count)
        .map(|r| {
            let voiced: Vec<f64> = pitches[r].iter().copied().filter(|&p| p > 0.0).collect();
            mean(&voiced)
        })
        .collect()
}

fn contour_interval(frame_count: usize, frame_duration: f64) -> f64 {
    if frame_count <= MAX_CONTOUR_POINTS {
        return frame_duration;
    }
    frame_duration * frame_count as f64 / MAX_CONTOUR_POINTS as f64
}

#[cfg(test)]
mod tests;
