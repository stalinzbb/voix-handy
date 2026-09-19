//! Practice sessions: record a speech, measure how it was delivered, coach it.
//!
//! Two deliberate departures from the dictation path, both because this measures
//! speech rather than typing it:
//! - capture uses `VadPolicy::Disabled`, so silences reach the analyzer as pauses;
//! - transcription keeps filler words, so they can be counted.
//!
//! A session is an ordinary history entry (WAV + transcript) with a
//! `practice_json` column. It is persisted *before* coaching runs, so a quit, a
//! dead endpoint or a cancelled request never costs the recording its analysis.

use crate::audio_toolkit::analysis::{analyze, DeliveryMetrics, Finding};
use crate::audio_toolkit::{save_wav_file, VadPolicy};
use crate::jev::{self, ContentJudgment};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, APPLE_INTELLIGENCE_PROVIDER_ID};
use log::warn;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, State};

const BINDING_ID: &str = "practice";
const SAMPLE_RATE: f64 = 16_000.0;

/// Vocalized hesitations only. "So" and "like" are words until the user says
/// otherwise via the custom filler list in settings.
const DEFAULT_FILLER_WORDS: &[&str] = &["um", "uh", "er", "ah", "erm", "hmm", "mm"];

/// Target speaking rate for a practiced talk. Single source for the coach prompt
/// and the UI label, so the two cannot disagree about whether a pace is fine.
pub const PACE_RANGE_WPM: (u32, u32) = (100, 130);

/// Reasoning models think before they write; a coaching answer over a long
/// transcript can legitimately take a couple of minutes. llm_client sets no
/// timeout of its own, so without this a dead endpoint hangs the session forever.
const COACHING_TIMEOUT: Duration = Duration::from_secs(180);

const PENDING_COACHING: &str = "Coaching did not complete.";

/// Stored as JSON in `transcription_history.practice_json`. New fields need
/// `#[serde(default)]` or older sessions stop decoding.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct PracticeData {
    pub metrics: DeliveryMetrics,
    #[serde(default)]
    pub coaching: Option<String>,
    #[serde(default)]
    pub coaching_error: Option<String>,
    #[serde(default)]
    pub coaching_model: Option<String>,
    /// Jev's content judgments. None when no TypeSafe key was set, or it failed.
    #[serde(default)]
    pub content: Option<ContentJudgment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PracticeSession {
    pub entry: HistoryEntry,
    pub data: PracticeData,
    /// Plain-language verdicts, most urgent first. Computed on read rather than
    /// stored, so retuning a threshold re-judges old sessions too.
    pub findings: Vec<Finding>,
    /// The speaker's own clause that states their point — only when Jev is
    /// confident which one it is. A shaky quote is worse than none.
    pub main_point_quote: Option<String>,
}

impl PracticeSession {
    fn new(entry: HistoryEntry, data: PracticeData) -> Self {
        let mut findings = data.metrics.findings(PACE_RANGE_WPM);
        if let Some(content) = &data.content {
            findings.extend(content.findings());
            findings.sort_by_key(|f| f.level); // stable: delivery before content
        }
        let main_point_quote = data
            .content
            .as_ref()
            .and_then(|c| c.confident_main_point())
            .map(str::to_string);
        Self {
            entry,
            data,
            findings,
            main_point_quote,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaceRange {
    pub min_wpm: u32,
    pub max_wpm: u32,
}

#[tauri::command]
#[specta::specta]
pub fn get_practice_pace_range() -> PaceRange {
    PaceRange {
        min_wpm: PACE_RANGE_WPM.0,
        max_wpm: PACE_RANGE_WPM.1,
    }
}

/// Fails with "Already recording" while dictation owns the microphone, and
/// dictation fails the same way while practice does — the recorder is shared.
#[tauri::command]
#[specta::specta]
pub async fn start_practice(
    recording_manager: State<'_, Arc<AudioRecordingManager>>,
) -> Result<(), String> {
    let rm = recording_manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        rm.try_start_recording(BINDING_ID, VadPolicy::Disabled)
            .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Discards the recording without transcribing or saving it.
#[tauri::command]
#[specta::specta]
pub async fn cancel_practice(
    recording_manager: State<'_, Arc<AudioRecordingManager>>,
) -> Result<(), String> {
    let rm = recording_manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        rm.stop_recording(BINDING_ID, rm.cancel_generation());
    })
    .await
    .map_err(|e| e.to_string())
}

/// Stops recording, transcribes, measures and saves. Coaching is a separate call
/// (`coach_practice_session`) so the numbers are on screen while the LLM thinks.
#[tauri::command]
#[specta::specta]
pub async fn stop_practice(
    app: AppHandle,
    recording_manager: State<'_, Arc<AudioRecordingManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<PracticeSession, String> {
    let rm = recording_manager.inner().clone();
    let tm = transcription_manager.inner().clone();
    let hm = history_manager.inner().clone();

    let blocking_app = app.clone();
    let session = tauri::async_runtime::spawn_blocking(move || {
        let app = blocking_app;
        let samples = rm
            .stop_recording(BINDING_ID, rm.cancel_generation())
            .ok_or("No practice recording is in progress")?;

        let transcript = tm
            .transcribe_keeping_fillers(samples.clone())
            .map_err(|e| format!("Transcription failed: {e}"))?;
        if transcript.trim().is_empty() {
            return Err("No speech was detected in the recording".to_string());
        }

        let metrics = analyze(&samples, SAMPLE_RATE, &transcript, &filler_words(&app));

        let file_name = format!("practice-{}.wav", chrono::Utc::now().timestamp());
        save_wav_file(hm.recordings_dir().join(&file_name), &samples)
            .map_err(|e| format!("Failed to save the recording: {e}"))?;

        let entry = hm
            .save_entry(file_name, transcript, false, None, None)
            .map_err(|e| e.to_string())?;
        let data = PracticeData {
            metrics,
            coaching_error: Some(PENDING_COACHING.to_string()),
            ..Default::default()
        };
        store(&hm, entry.id, &data)?;
        Ok(PracticeSession::new(entry, data))
    })
    .await
    .map_err(|e| e.to_string())??;

    // Already saved above, so a TypeSafe outage costs the content cards and
    // nothing else.
    Ok(with_content(&app, &history_manager, session).await)
}

/// Adds Jev's content judgments to a session that has none yet. Returns the
/// session unchanged when no key is set or the call fails.
async fn with_content(
    app: &AppHandle,
    hm: &HistoryManager,
    mut session: PracticeSession,
) -> PracticeSession {
    let Some(key) = typesafe_api_key(app) else {
        return session;
    };
    if session.data.content.is_some() {
        return session; // its fillers are already in the counts; never add them twice
    }
    match jev::judge(&session.entry.transcription_text, &filler_words(app), &key).await {
        Ok(content) => {
            let metrics = &mut session.data.metrics;
            for span in content.counted_fillers() {
                *metrics
                    .filler_counts
                    .entry(span.phrase.clone())
                    .or_insert(0) += 1;
            }
            let minutes = metrics.speech_span_seconds / 60.0;
            if minutes > 0.0 {
                metrics.fillers_per_minute = metrics.total_fillers() as f64 / minutes;
            }
            session.data.content = Some(content);
            if let Err(e) = store(hm, session.entry.id, &session.data) {
                warn!("Failed to save content analysis: {e}");
            }
            PracticeSession::new(session.entry, session.data)
        }
        Err(e) => {
            warn!("Content analysis failed: {e}");
            session
        }
    }
}

const TYPESAFE_KEY_ID: &str = "typesafe";

fn typesafe_api_key(app: &AppHandle) -> Option<String> {
    get_settings(app)
        .post_process_api_keys
        .get(TYPESAFE_KEY_ID)
        .filter(|k| !k.trim().is_empty())
        .cloned()
}

/// Stored beside the other provider keys. An empty key switches content analysis off.
#[tauri::command]
#[specta::specta]
pub fn set_typesafe_api_key(app: AppHandle, api_key: String) {
    let mut settings = get_settings(&app);
    settings
        .post_process_api_keys
        .insert(TYPESAFE_KEY_ID.to_string(), api_key.trim().to_string());
    crate::settings::write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn has_typesafe_api_key(app: AppHandle) -> bool {
    typesafe_api_key(&app).is_some()
}

/// Runs content analysis on a session recorded before a key was set.
#[tauri::command]
#[specta::specta]
pub async fn analyze_practice_content(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<PracticeSession, String> {
    let session = sessions(&history_manager)?
        .into_iter()
        .find(|s| s.entry.id == id)
        .ok_or("Practice session not found")?;
    Ok(with_content(&app, &history_manager, session).await)
}

/// Runs (or re-runs) coaching for a saved session. A coaching failure is data,
/// not an `Err`: it is stored on the session and shown beside its metrics.
#[tauri::command]
#[specta::specta]
pub async fn coach_practice_session(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<PracticeSession, String> {
    let hm = history_manager.inner().clone();
    let mut session = sessions(&hm)?
        .into_iter()
        .find(|s| s.entry.id == id)
        .ok_or("Practice session not found")?;

    match request_coaching(&app, &session).await {
        Ok((text, model)) => {
            session.data.coaching = Some(text);
            session.data.coaching_model = Some(model);
            session.data.coaching_error = None;
        }
        Err(error) => session.data.coaching_error = Some(error),
    }
    store(&hm, id, &session.data)?;
    Ok(session)
}

#[tauri::command]
#[specta::specta]
pub async fn get_practice_sessions(
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<PracticeSession>, String> {
    sessions(history_manager.inner())
}

fn store(hm: &HistoryManager, id: i64, data: &PracticeData) -> Result<(), String> {
    let json = serde_json::to_string(data).map_err(|e| e.to_string())?;
    hm.set_practice_json(id, &json).map_err(|e| e.to_string())
}

/// One undecodable session drops that session only, never the whole history.
fn sessions(hm: &HistoryManager) -> Result<Vec<PracticeSession>, String> {
    let rows = hm.get_practice_entries().map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(|(entry, json)| match serde_json::from_str(&json) {
            Ok(data) => Some(PracticeSession::new(entry, data)),
            Err(e) => {
                warn!("Skipping undecodable practice session {}: {e}", entry.id);
                None
            }
        })
        .collect())
}

pub(crate) fn filler_words(app: &AppHandle) -> Vec<String> {
    get_settings(app)
        .custom_filler_words
        .unwrap_or_else(|| DEFAULT_FILLER_WORDS.iter().map(|w| w.to_string()).collect())
}

async fn request_coaching(
    app: &AppHandle,
    session: &PracticeSession,
) -> Result<(String, String), String> {
    let settings = get_settings(app);
    let provider = settings
        .active_post_process_provider()
        .cloned()
        .ok_or("No AI provider is selected. Choose one in Post Process settings.")?;
    if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
        // ponytail: coaching is a long free-form answer; the Apple Intelligence
        // bridge is wired for short structured rewrites. Add when someone asks.
        return Err("Apple Intelligence can't run coaching yet. Choose another provider in Post Process settings.".to_string());
    }
    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() {
        return Err(format!(
            "No model is configured for provider '{}'.",
            provider.label
        ));
    }
    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    // The verdicts are the ones already on screen. Handing them over keeps the
    // coach from calling a pace slow that the card beside it calls fine.
    let verdicts: Vec<String> = session
        .findings
        .iter()
        .map(|f| format!("{}: {}", f.metric, f.note))
        .collect();
    let user_content = format!(
        "TRANSCRIPT\n{}\n\n{}\n\nAPP VERDICTS (already shown to the speaker; do not contradict them)\n{}",
        session.entry.transcription_text.trim(),
        session.data.metrics.summary_text(),
        if verdicts.is_empty() {
            "none — delivery was not measurable".to_string()
        } else {
            verdicts.join("\n")
        }
    );

    // Reasoning stays on: unlike dictation cleanup this is not a typing hot path,
    // and a critique is better for the thinking.
    let request = crate::llm_client::send_chat_completion_with_schema(
        &provider,
        api_key,
        &model,
        user_content,
        Some(coach_prompt()),
        None,
        false,
    );
    match tokio::time::timeout(COACHING_TIMEOUT, request).await {
        Err(_) => Err(format!(
            "The AI provider did not answer within {} seconds.",
            COACHING_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(e),
        Ok(Ok(Some(text))) if !text.trim().is_empty() => Ok((text, model)),
        Ok(Ok(_)) => Err("The AI provider returned an empty answer.".to_string()),
    }
}

/// ponytail: hardcoded. Promote to an editable setting once it has been iterated
/// against real recordings — an editable prompt that is still wrong is just a
/// wrong prompt with more surface area.
fn coach_prompt() -> String {
    let (min, max) = PACE_RANGE_WPM;
    format!(
        "You are a speech coach giving feedback someone will read in under a minute, right \
after practising. Below is a verbatim speech-to-text transcript, delivery metrics measured \
from the audio, and the verdicts the app already showed them.

Write in plain, everyday words. No jargon: say \"your voice stays on one note\", not \"low \
pitch standard deviation\". Short sentences. Talk to them as \"you\". The whole answer must \
be under 200 words.

Use exactly these Markdown sections, in this order:

## Bottom line
One sentence: what this talk was trying to say, and whether it landed. If you cannot find \
a point, say so plainly — that is the most useful thing you can tell them.

## Do next time
At most 3 bullets, most important first. Each bullet is one concrete action in under 20 \
words, starting with a verb. Mix content and delivery by what matters most. Where a \
number backs it up, put the number in brackets at the end, like (93 words a minute).

## What worked
At most 2 bullets. Quote a short phrase of theirs if you can. Skip this section rather \
than invent praise.

## Try this drill
One drill, two sentences, for the first bullet above.

Rules. The transcript may contain recognition mistakes — ignore grammar-level noise. \
Only the figures in the metrics block are measured; anything else is your inference, so \
say \"it sounds like\". Do not contradict the app verdicts, and do not criticise anything \
they mark as fine. For reference: {min}–{max} words a minute is the target pace; 60–75% \
voiced is normal; pauses at the end of a thought help, pauses mid-thought hurt; more than \
about 4 fillers a minute distracts. If delivery metrics are unavailable, say so in one \
line under Bottom line and coach the content only."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coach_prompt_quotes_the_shared_pace_range() {
        assert!(coach_prompt().contains("100–130 words a minute"));
    }

    /// Sessions saved before a field existed must keep decoding.
    #[test]
    fn practice_data_decodes_without_optional_fields() {
        let json = serde_json::json!({ "metrics": DeliveryMetrics::default() }).to_string();
        let data: PracticeData = serde_json::from_str(&json).unwrap();
        assert!(data.coaching.is_none() && data.coaching_error.is_none());
    }
}
