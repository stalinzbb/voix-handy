use super::*;

const SAMPLE_RATE: f64 = 16_000.0;

fn run(pcm: &[f32], transcript: &str, fillers: &[&str]) -> DeliveryMetrics {
    let fillers: Vec<String> = fillers.iter().map(|s| s.to_string()).collect();
    analyze(pcm, SAMPLE_RATE, transcript, &fillers)
}

fn assert_close(actual: f64, expected: f64, accuracy: f64) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{actual} is not within {accuracy} of {expected}"
    );
}

// --- Signal helpers ---

fn sine_at(frequency: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
    let count = (SAMPLE_RATE * seconds) as usize;
    (0..count)
        .map(|i| {
            amplitude
                * (2.0 * std::f64::consts::PI * frequency * i as f64 / SAMPLE_RATE).sin() as f32
        })
        .collect()
}

fn sine(frequency: f64, seconds: f64) -> Vec<f32> {
    sine_at(frequency, seconds, 0.5)
}

fn silence(seconds: f64) -> Vec<f32> {
    vec![0.0; (SAMPLE_RATE * seconds) as usize]
}

/// Deterministic PRNG so a failing audio test reproduces exactly.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_unit_interval(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Broadband noise at a speech-like level.
fn noise_at(seconds: f64, seed: u64, amplitude: f32) -> Vec<f32> {
    let mut generator = SplitMix64(seed);
    (0..(SAMPLE_RATE * seconds) as usize)
        .map(|_| amplitude * (generator.next_unit_interval() as f32 * 2.0 - 1.0))
        .collect()
}

fn noise(seconds: f64, seed: u64) -> Vec<f32> {
    noise_at(seconds, seed, 0.2)
}

fn concat(parts: &[Vec<f32>]) -> Vec<f32> {
    parts.concat()
}

// --- Pitch ---

#[test]
fn recovers_pitch_of_pure_tone() {
    assert_close(run(&sine(440.0, 1.0), "", &[]).mean_pitch_hz, 440.0, 5.0);
}

/// A period multiple correlates about as well as the period itself, so a naive
/// peak pick reports half the pitch. Low tones have the most room to halve.
#[test]
fn does_not_report_subharmonic_for_low_tone() {
    assert_close(run(&sine(120.0, 1.0), "", &[]).mean_pitch_hz, 120.0, 3.0);
}

#[test]
fn constant_tone_scores_as_monotone() {
    let m = run(&sine(200.0, 2.0), "", &[]);
    assert_close(m.pitch_std_dev_hz, 0.0, 1.0);
    assert_close(m.pitch_range_hz, 0.0, 2.0);
}

#[test]
fn pitch_variation_is_reported_as_variety() {
    let m = run(&concat(&[sine(150.0, 1.0), sine(225.0, 1.0)]), "", &[]);
    assert!(m.pitch_std_dev_hz > 20.0);
    assert_close(m.pitch_range_hz, 75.0, 10.0);
}

// --- Pauses ---

#[test]
fn detects_inserted_silence_gaps() {
    let pcm = concat(&[
        noise(2.0, 1),
        silence(1.0),
        noise(2.0, 2),
        silence(1.0),
        noise(2.0, 3),
    ]);
    let m = run(&pcm, "", &[]);

    assert_eq!(m.pauses.len(), 2);
    assert_close(m.pauses[0].start_seconds, 2.0, 0.05);
    assert_close(m.pauses[0].duration_seconds, 1.0, 0.05);
    assert_close(m.pauses[1].start_seconds, 5.0, 0.05);
    assert_close(m.pauses[1].duration_seconds, 1.0, 0.05);
    assert_close(m.total_pause_seconds, 2.0, 0.1);
    assert_close(m.longest_pause_seconds, 1.0, 0.05);
    assert_close(m.duration_seconds, 8.0, 0.01);
}

/// A 0.2s gap is a breath, not a pause.
#[test]
fn ignores_gaps_shorter_than_threshold() {
    let m = run(
        &concat(&[noise(1.0, 1), silence(0.2), noise(1.0, 2)]),
        "",
        &[],
    );
    assert!(m.pauses.is_empty());
}

/// Hitting record and waiting is not a rhetorical pause, and it must not drag the
/// pace numbers down either.
#[test]
fn leading_and_trailing_silence_is_not_a_pause() {
    let m = run(
        &concat(&[silence(3.0), noise(2.0, 1), silence(3.0)]),
        "",
        &[],
    );
    assert!(m.pauses.is_empty());
    assert_close(m.duration_seconds, 8.0, 0.01);
    assert_close(m.speech_span_seconds, 2.0, 0.1);
}

#[test]
fn flags_pauses_of_two_seconds_or_more() {
    let m = run(
        &concat(&[noise(1.0, 1), silence(2.5), noise(1.0, 2)]),
        "",
        &[],
    );
    assert_eq!(m.pauses.len(), 1);
    assert_eq!(m.long_pauses().len(), 1);
    assert_close(m.long_pauses()[0].duration_seconds, 2.5, 0.05);
}

// --- Pace ---

/// 60 words over a 2s + 2s silence + 2s span. Overall pace is measured across the
/// whole 6s span; articulation excludes the pause, so it must be higher.
#[test]
fn pace_separates_overall_rate_from_articulation_rate() {
    let pcm = concat(&[noise(2.0, 1), silence(2.0), noise(2.0, 2)]);
    let transcript: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
    let m = run(&pcm, &transcript.join(" "), &[]);

    assert_eq!(m.total_words, 60);
    assert_close(m.words_per_minute, 600.0, 20.0);
    assert_close(m.articulation_rate, 900.0, 30.0);
    assert!(m.articulation_rate > m.words_per_minute);
    assert_close(m.speaking_ratio, 2.0 / 3.0, 0.05);
}

// --- Fillers ---

fn counts(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
    pairs.iter().map(|(w, n)| (w.to_string(), *n)).collect()
}

#[test]
fn counts_fillers_per_type() {
    let m = run(
        &noise(60.0, 1),
        "um so like the plan um is uh ready",
        &["um", "uh", "so", "like"],
    );
    assert_eq!(
        m.filler_counts,
        counts(&[("um", 2), ("so", 1), ("like", 1), ("uh", 1)])
    );
    assert_eq!(m.total_fillers(), 5);
    assert_close(m.fillers_per_minute, 5.0, 0.3);
}

#[test]
fn only_counts_words_on_the_configured_list() {
    let m = run(&noise(10.0, 1), "um so like the plan um", &["um", "uh"]);
    assert_eq!(m.filler_counts, counts(&[("um", 2)]));
}

#[test]
fn filler_matching_ignores_case_and_punctuation() {
    let m = run(
        &noise(10.0, 1),
        "Um, the plan... UH! and uh,",
        &["um", "uh"],
    );
    assert_eq!(m.filler_counts, counts(&[("um", 1), ("uh", 2)]));
}

// --- Signal quality gate ---

/// The failure this gate exists for. A recording of nothing but room tone has its
/// adaptive threshold fitted to the room tone, so it reads as continuous confident
/// speech. Observed live: 88s of ambient noise reported 0 pauses and 99.6% voiced.
#[test]
fn uniform_room_noise_is_rejected() {
    let m = run(
        &noise_at(30.0, 7, 0.05),
        "so there's so many notes and single ones",
        &[],
    );
    assert!(!m.quality.is_reliable);
    assert!(m.quality.signal_to_noise_db < 12.0);
    assert!(m.quality.warning.is_some());
}

/// Speech-like input — loud bursts over a quiet floor — must still pass, or the
/// gate is just an off switch.
#[test]
fn speech_over_quiet_floor_is_accepted() {
    let mut pcm = Vec::new();
    for burst in 0..6 {
        pcm.extend(noise_at(1.5, burst, 0.3));
        pcm.extend(noise_at(0.8, burst + 50, 0.002));
    }
    let m = run(&pcm, "the plan is ready and the numbers support it", &[]);

    assert!(
        m.quality.is_reliable,
        "warning was: {:?}",
        m.quality.warning
    );
    assert!(m.quality.signal_to_noise_db > 12.0);
    assert!(m.quality.warning.is_none());
}

/// Full-scale square wave: loud, well separated, and completely distorted.
#[test]
fn clipped_input_is_rejected() {
    let pcm: Vec<f32> = (0..(SAMPLE_RATE * 3.0) as usize)
        .map(|i| if i % 100 < 50 { 1.0 } else { -1.0 })
        .collect();
    let m = run(&pcm, "hello", &[]);

    assert!(!m.quality.is_reliable);
    assert!(m.quality.clipped_sample_ratio > 0.005);
    assert!(m.quality.warning.unwrap().contains("clipping"));
}

#[test]
fn faint_recording_is_rejected() {
    let pcm = concat(&[
        sine_at(150.0, 2.0, 0.0015),
        silence(1.0),
        sine_at(150.0, 2.0, 0.0015),
    ]);
    let m = run(&pcm, "barely audible", &[]);

    assert!(!m.quality.is_reliable);
    assert!(m.quality.warning.unwrap().contains("too quiet"));
}

/// The point of the gate: an unreliable recording must not hand numbers to the
/// coach, because a model given caveated measurements still reasons about them.
#[test]
fn summary_withholds_numbers_when_unreliable() {
    let summary = run(&noise_at(30.0, 7, 0.05), "so there's so many notes", &[]).summary_text();

    assert!(summary.contains("DELIVERY METRICS UNAVAILABLE"));
    assert!(!summary.contains("WPM"));
    assert!(!summary.contains("Articulation rate"));
    assert!(!summary.contains("Pauses"));
}

// --- Degenerate input ---

#[test]
fn empty_audio_produces_empty_metrics() {
    assert_eq!(run(&[], "anything", &["um"]), DeliveryMetrics::default());
}

/// Silence yields no measurements, but must still say *why* — "too quiet" sends the
/// user to fix their microphone, where a bare empty result tells them nothing.
#[test]
fn pure_silence_produces_empty_metrics_with_a_reason() {
    let m = run(&silence(3.0), "", &[]);

    assert_eq!(m.total_words, 0);
    assert_eq!(m.speaking_seconds, 0.0);
    assert!(m.pauses.is_empty());
    assert!(m.pitch_contour.is_empty());
    assert!(!m.quality.is_reliable);
    assert!(m.quality.warning.unwrap().contains("too quiet"));
}

// --- Serialization and rendering ---

#[test]
fn metrics_round_trip_through_json() {
    let pcm = concat(&[noise(2.0, 1), silence(1.0), noise(2.0, 2)]);
    let m = run(&pcm, "um the plan", &["um"]);

    let decoded: DeliveryMetrics =
        serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();

    // serde_json without `float_roundtrip` may land one ULP off, so floats are
    // compared approximately rather than with a whole-struct assert_eq.
    assert_eq!(decoded.pauses.len(), m.pauses.len());
    assert_eq!(decoded.filler_counts, m.filler_counts);
    assert_eq!(decoded.total_words, m.total_words);
    assert_eq!(decoded.quality.is_reliable, m.quality.is_reliable);
    assert_eq!(decoded.energy_contour.len(), m.energy_contour.len());
    assert_close(decoded.words_per_minute, m.words_per_minute, 1e-9);
    assert_close(decoded.mean_level_db, m.mean_level_db, 1e-9);
}

/// Sessions stored before a field existed must keep decoding. `quality` is the
/// worked example: missing means "never assessed", not "throw the session away".
#[test]
fn missing_quality_decodes_as_unknown() {
    let mut value = serde_json::to_value(run(&noise(2.0, 1), "hello", &[])).unwrap();
    value.as_object_mut().unwrap().remove("quality");

    let decoded: DeliveryMetrics = serde_json::from_value(value).unwrap();

    assert_eq!(decoded.quality, SignalQuality::default());
    assert_eq!(decoded.total_words, 1);
}

#[test]
fn summary_text_cites_the_measurements() {
    let pcm = concat(&[noise(2.0, 1), silence(2.5), noise(2.0, 2)]);
    let summary = run(&pcm, "um the plan is ready", &["um"]).summary_text();

    // The coaching prompt asks the model to quote these back, so they have to be present.
    assert!(summary.contains("WPM"));
    assert!(summary.contains("Articulation rate"));
    assert!(summary.contains("um x1"));
    assert!(summary.contains("Pauses >= 2s: 1"));
    assert!(!summary.to_lowercase().contains("nan"));
    assert!(!summary.contains("inf"));
}

/// 120s of 20ms frames is 6000 raw points; charts get a bounded series back.
#[test]
fn contour_is_downsampled_for_plotting() {
    let m = run(&noise(120.0, 1), "", &[]);

    assert_eq!(m.energy_contour.len(), 240);
    assert!(m.contour_interval_seconds > 0.0);
    let span = m.contour_interval_seconds * m.energy_contour.len() as f64;
    assert_close(span, m.duration_seconds, 1.0);
}

/// Energy frames are 20ms and pitch windows 50ms, but there is exactly one
/// `contour_interval_seconds` — so both contours must share its grid. Short
/// recordings are the regression case.
#[test]
fn pitch_and_energy_contours_share_one_time_grid() {
    for seconds in [3.0, 8.0, 30.0] {
        let m = run(&noise(seconds, 7), "", &[]);

        assert_eq!(
            m.pitch_contour.len(),
            m.energy_contour.len(),
            "contours diverge at {seconds}s"
        );
        let span = m.contour_interval_seconds * m.pitch_contour.len() as f64;
        assert_close(span, m.duration_seconds, 1.0);
    }
}

/// A bucket holding one 150 Hz window and one unvoiced zero must not average to
/// 75 Hz — a pitch that was never spoken.
#[test]
fn pitch_resampling_never_invents_intermediate_pitches() {
    let mut pcm = Vec::new();
    for _ in 0..30 {
        pcm.extend(sine(150.0, 0.5));
        pcm.extend(silence(0.5));
    }
    let m = run(&pcm, "", &[]);

    assert!(
        m.pitch_contour.iter().any(|&hz| hz > 0.0),
        "expected voiced buckets"
    );
    assert!(m.pitch_contour.contains(&0.0), "unvoiced gaps must stay 0");
    for &hz in m.pitch_contour.iter().filter(|&&hz| hz > 0.0) {
        assert!(
            hz > 100.0,
            "bucket averaged unvoiced zeros into a voiced pitch"
        );
    }
}

/// Found on real speech: room tone is low-passed, so neighbouring samples
/// correlate and the quiet gaps between phrases "measure" as 300–500 Hz. That
/// tripled the reported pitch spread and made every speaker read as expressive.
/// Pitch must only be estimated where there is speech.
#[test]
fn room_tone_between_phrases_is_not_pitched() {
    let mut generator = SplitMix64(11);
    let mut room_tone = |seconds: f64| -> Vec<f32> {
        let raw: Vec<f32> = (0..(SAMPLE_RATE * seconds) as usize + 8)
            .map(|_| generator.next_unit_interval() as f32 * 2.0 - 1.0)
            .collect();
        // Moving average = crude low-pass, like a real room and microphone.
        raw.windows(8)
            .map(|w| 0.004 * w.iter().sum::<f32>() / 8.0)
            .collect()
    };
    let mut pcm = Vec::new();
    for _ in 0..6 {
        pcm.extend(sine(150.0, 1.0));
        pcm.extend(room_tone(1.0));
    }
    let m = run(&pcm, "", &[]);

    assert!(m.quality.is_reliable, "warning: {:?}", m.quality.warning);
    assert_close(m.mean_pitch_hz, 150.0, 3.0);
    assert!(m.pitch_range_hz < 10.0, "range was {}", m.pitch_range_hz);
}

// --- Findings ---

fn finding(m: &DeliveryMetrics, metric: &str) -> (Level, String) {
    let f = m
        .findings((100, 130))
        .into_iter()
        .find(|f| f.metric == metric)
        .unwrap();
    (f.level, f.note)
}

#[test]
fn findings_put_the_most_urgent_first_and_judge_pitch_relative_to_the_voice() {
    let reliable = SignalQuality {
        is_reliable: true,
        warning: None,
        ..Default::default()
    };
    let m = DeliveryMetrics {
        words_per_minute: 92.0,
        fillers_per_minute: 6.0,
        mean_pitch_hz: 220.0,
        pitch_std_dev_hz: 14.0, // 6% of 220 Hz: monotone for this voice
        dynamic_range_db: 20.0,
        quality: reliable.clone(),
        ..Default::default()
    };

    assert_eq!(finding(&m, "pace"), (Level::Watch, "bit_slow".into()));
    assert_eq!(finding(&m, "fillers"), (Level::WorkOn, "many".into()));
    assert_eq!(finding(&m, "pitch"), (Level::WorkOn, "monotone".into()));
    assert_eq!(finding(&m, "pauses"), (Level::Good, "none_long".into()));
    let levels: Vec<Level> = m.findings((100, 130)).iter().map(|f| f.level).collect();
    assert!(
        levels.windows(2).all(|w| w[0] <= w[1]),
        "not sorted: {levels:?}"
    );

    // The same 14 Hz of deviation is healthy variety on a 100 Hz voice.
    let low_voice = DeliveryMetrics {
        mean_pitch_hz: 100.0,
        ..m
    };
    assert_eq!(finding(&low_voice, "pitch"), (Level::Good, "varied".into()));
}

#[test]
fn unreliable_recordings_get_no_findings() {
    let m = run(&noise_at(30.0, 7, 0.05), "some words", &[]);
    assert!(m.findings((100, 130)).is_empty());
}
