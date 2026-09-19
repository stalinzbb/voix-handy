//! Content judgments from TypeSafe's Jev model.
//!
//! Jev does not generate text; it returns typed answers with probabilities. That
//! fits the three things a word list and a free-text coach cannot do:
//! - decide whether *this* "like" / "so" / "I guess" is a filler (context decides);
//! - rate content on fixed, described levels, so sessions are comparable;
//! - pick which of the speaker's own clauses states the point, so the quote shown
//!   back to them is copied, never written.
//!
//! Code keeps everything Jev is documented to be bad at: counting, thresholds,
//! arithmetic. Only the transcript text is sent — never audio.

use crate::audio_toolkit::analysis::{Finding, Level};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use specta::Type;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-latest";
const TIMEOUT: Duration = Duration::from_secs(20);

/// Words that are fillers only sometimes. Longest phrases first so "kind of"
/// wins over a bare "kind".
const AMBIGUOUS_PHRASES: &[&[&str]] = &[
    &["you", "know"],
    &["i", "mean"],
    &["i", "guess"],
    &["kind", "of"],
    &["sort", "of"],
    &["like"],
    &["so"],
    &["actually"],
    &["basically"],
    &["literally"],
    &["right"],
    &["well"],
    &["just"],
    &["okay"],
];
const MAX_CANDIDATES: usize = 40;
const CONTEXT_WORDS: usize = 10;
const CLAUSE_WORDS: usize = 14;

// Judgement thresholds — tune against real recordings.
/// Probability above which an ambiguous word counts as a filler. "just" in a real
/// session came back at exactly 0.50: genuinely undecidable, so it must not count.
pub const FILLER_PROBABILITY: f64 = 0.7;
/// A content score spread this thinly across levels is a shrug; show nothing.
const MIN_SCORE_CONFIDENCE: f64 = 0.3;
const MAIN_POINT_PRESENT: f64 = 0.6;
const MAIN_POINT_ABSENT: f64 = 0.4;
const MIN_QUOTE_CONFIDENCE: f64 = 0.5;

/// Content dimensions, each scored 0–3 against the levels in `content_questions`.
const DIMENSIONS: &[&str] = &[
    "opening",
    "structure",
    "specificity",
    "closing",
    "conviction",
];

/// An ambiguous word Jev was asked about. Token indices are positions in
/// `transcript.split_whitespace()`, which the UI reproduces to highlight them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct FillerSpan {
    pub first_token: u32,
    pub last_token: u32,
    pub phrase: String,
    pub probability: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct JevScore {
    pub score: f64,
    pub confidence: f64,
}

/// Raw answers, stored as-is. Verdicts are derived on read (`findings`), so a
/// threshold change re-judges old sessions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct ContentJudgment {
    #[serde(default)]
    pub filler_spans: Vec<FillerSpan>,
    #[serde(default)]
    pub scores: BTreeMap<String, JevScore>,
    #[serde(default)]
    pub has_main_point: f64,
    /// The speaker's own clause, copied from the transcript.
    #[serde(default)]
    pub main_point: Option<String>,
    #[serde(default)]
    pub main_point_confidence: f64,
}

impl ContentJudgment {
    pub fn counted_fillers(&self) -> impl Iterator<Item = &FillerSpan> {
        self.filler_spans
            .iter()
            .filter(|s| s.probability >= FILLER_PROBABILITY)
    }

    /// The quote is only worth showing when Jev both thinks a point exists and is
    /// reasonably sure which clause it is.
    pub fn confident_main_point(&self) -> Option<&str> {
        (self.has_main_point >= MAIN_POINT_PRESENT
            && self.main_point_confidence >= MIN_QUOTE_CONFIDENCE)
            .then_some(self.main_point.as_deref())
            .flatten()
    }

    pub fn findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if self.has_main_point < MAIN_POINT_ABSENT {
            findings.push(finding("point", Level::WorkOn, "missing"));
        } else if self.has_main_point >= MAIN_POINT_PRESENT {
            findings.push(finding("point", Level::Good, "found"));
        }
        for dimension in DIMENSIONS {
            let Some(s) = self.scores.get(*dimension) else {
                continue;
            };
            if s.confidence < MIN_SCORE_CONFIDENCE {
                continue;
            }
            // Jev's scores are weakly calibrated as numbers, so they are only ever
            // read as three coarse steps.
            let (level, note) = if s.score < 1.0 {
                (Level::WorkOn, "low")
            } else if s.score < 2.0 {
                (Level::Watch, "mid")
            } else {
                (Level::Good, "high")
            };
            findings.push(finding(dimension, level, note));
        }
        findings
    }
}

fn finding(metric: &str, level: Level, note: &str) -> Finding {
    Finding {
        metric: metric.to_string(),
        level,
        note: note.to_string(),
    }
}

fn normalize(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Every occurrence of an ambiguous phrase, as (first token, last token, phrase).
/// Words already on the user's filler list are skipped — those are counted
/// unconditionally by the analyzer, and must not be counted twice.
fn filler_candidates(tokens: &[&str], listed: &HashSet<String>) -> Vec<(usize, usize, String)> {
    let normalized: Vec<String> = tokens.iter().map(|t| normalize(t)).collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < normalized.len() && found.len() < MAX_CANDIDATES {
        let hit = AMBIGUOUS_PHRASES.iter().find(|phrase| {
            phrase
                .iter()
                .enumerate()
                .all(|(k, word)| normalized.get(i + k).is_some_and(|t| t == word))
        });
        match hit {
            Some(phrase) if !listed.contains(&phrase.join(" ")) => {
                found.push((i, i + phrase.len() - 1, phrase.join(" ")));
                i += phrase.len();
            }
            _ => i += 1,
        }
    }
    found
}

/// Clause-sized units for "which one states the point". Speech-to-text often
/// returns a whole take as one run-on sentence, so sentences are cut further at
/// commas and then at a fixed length; a unit holding two thoughts cannot be the
/// answer to "which thought is the point".
fn clauses(transcript: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sentence in transcript.split_inclusive(['.', '!', '?']) {
        for part in sentence.split_inclusive(',') {
            let words: Vec<&str> = part.split_whitespace().collect();
            for chunk in words.chunks(CLAUSE_WORDS) {
                out.push(chunk.join(" "));
            }
        }
    }
    out.retain(|c| !c.is_empty());
    out
}

fn score_question(instructions: &str, levels: [&str; 4]) -> Value {
    json!({ "type": "score", "instructions": instructions, "criteria": levels })
}

fn content_questions() -> Vec<(&'static str, Value)> {
    vec![
        (
            "opening",
            score_question(
                "How does `transcript` begin — its first sentence or two?",
                [
                    "Starts mid-thought, off topic, or abandons its first sentence",
                    "Introduces the topic in general terms without saying why it matters",
                    "States the topic and why the listener should care",
                    "Opens with the point itself, or a concrete hook that leads straight into it",
                ],
            ),
        ),
        (
            "structure",
            score_question(
                "How easy is it to follow the order of ideas in `transcript`?",
                [
                    "Ideas arrive in no discernible order; thoughts are abandoned mid-sentence",
                    "There is a rough thread, but it wanders or restarts",
                    "A clear sequence of ideas, though the transitions are not signposted",
                    "A clear sequence with explicit signposts (first, second, so, in short)",
                ],
            ),
        ),
        (
            "specificity",
            score_question(
                "How concrete is what the speaker says in `transcript`?",
                [
                    "Only general statements; no example, number, name or concrete detail",
                    "Mostly general, with one concrete detail",
                    "Several concrete details, examples or numbers support the claims",
                    "Every main claim is backed by a concrete example, number or named case",
                ],
            ),
        ),
        (
            "closing",
            score_question(
                "How does `transcript` end?",
                [
                    "Trails off, or ends on a hedge such as 'I guess' or 'or whatever'",
                    "Stops without a conclusion",
                    "Ends by restating the point",
                    "Ends with a clear ask, decision or next step for the listener",
                ],
            ),
        ),
        (
            "conviction",
            score_question(
                "How committed does the speaker sound to their own statements in `transcript`?",
                [
                    "Most statements are hedged or qualified (maybe, I guess, kind of, I think)",
                    "Frequent hedging alongside some plain statements",
                    "Mostly plain statements with occasional hedging",
                    "States things plainly and owns them",
                ],
            ),
        ),
    ]
}

struct Request {
    body: Value,
    candidates: Vec<(usize, usize, String)>,
    clauses: Vec<String>,
}

fn build_request(transcript: &str, listed_fillers: &[String]) -> Request {
    let tokens: Vec<&str> = transcript.split_whitespace().collect();
    let listed: HashSet<String> = listed_fillers.iter().map(|w| w.to_lowercase()).collect();
    let candidates = filler_candidates(&tokens, &listed);
    let clauses = clauses(transcript);

    let mut candidate_state = Map::new();
    let mut questions = Map::new();
    for (n, (first, last, _)) in candidates.iter().enumerate() {
        let id = format!("C{n:02}");
        let before = tokens[first.saturating_sub(CONTEXT_WORDS)..*first].join(" ");
        let after = tokens[last + 1..(last + 1 + CONTEXT_WORDS).min(tokens.len())].join(" ");
        let context = format!("{before} [[{}]] {after}", tokens[*first..=*last].join(" "));
        candidate_state.insert(id.clone(), json!({ "context": context.trim() }));
        questions.insert(
            format!("filler_{id}"),
            json!({
                "type": "noul",
                "instructions": format!(
                    "In `candidates.{id}.context`, the phrase inside [[ ]] was spoken aloud. \
                     Is it a filler or verbal hedge in this sentence?"
                ),
                "criteria": {
                    "true": "It carries no meaning here: deleting it leaves the sentence saying the \
                             same thing (e.g. 'there's [[like]] this little shop', 'or any other \
                             place [[I guess]]').",
                    "false": "It does real work here: a verb, a comparison, a connective linking \
                              cause and result, or an adverb that changes the meaning (e.g. 'I \
                              [[like]] this', 'it looks [[like]] rain', 'it was late [[so]] we left')."
                }
            }),
        );
    }

    let clause_state: Map<String, Value> = clauses
        .iter()
        .enumerate()
        .map(|(n, c)| (format!("S{n:02}"), json!(c)))
        .collect();
    let clause_options: Map<String, Value> = clause_state
        .keys()
        .map(|k| (k.clone(), Value::Null))
        .collect();

    questions.insert(
        "has_main_point".into(),
        json!({
            "type": "noul",
            "instructions": "Does `transcript` state what the speaker wants the listener to know, \
                             believe or do?"
        }),
    );
    if clause_options.len() >= 2 {
        questions.insert(
            "main_point".into(),
            json!({
                "type": "choice",
                "instructions": "Which entry of `clauses` best states the speaker's main point?",
                "criteria": clause_options
            }),
        );
    }
    for (id, question) in content_questions() {
        questions.insert(id.into(), question);
    }

    Request {
        body: json!({
            "model": MODEL,
            "state": {
                "transcript": transcript,
                "clauses": clause_state,
                "candidates": candidate_state
            },
            "questions": questions
        }),
        candidates,
        clauses,
    }
}

fn parse_response(request: &Request, response: &Value) -> ContentJudgment {
    let answers = &response["answers"];
    let number = |id: &str, field: &str| answers[id][field].as_f64().unwrap_or(0.0);

    let filler_spans = request
        .candidates
        .iter()
        .enumerate()
        .map(|(n, (first, last, phrase))| FillerSpan {
            first_token: *first as u32,
            last_token: *last as u32,
            phrase: phrase.clone(),
            probability: number(&format!("filler_C{n:02}"), "noul"),
        })
        .collect();

    let scores = DIMENSIONS
        .iter()
        .filter(|id| answers[**id]["score"].is_number())
        .map(|id| {
            (
                id.to_string(),
                JevScore {
                    score: number(id, "score"),
                    confidence: number(id, "confidence"),
                },
            )
        })
        .collect();

    // Copied out of the transcript by index — the quote cannot be invented.
    let main_point = answers["main_point"]["choice"]
        .as_str()
        .and_then(|id| id.strip_prefix('S'))
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|n| request.clauses.get(n).cloned())
        // A single clause needs no choosing.
        .or_else(|| (request.clauses.len() == 1).then(|| request.clauses[0].clone()));

    ContentJudgment {
        filler_spans,
        scores,
        has_main_point: number("has_main_point", "noul"),
        main_point_confidence: if request.clauses.len() == 1 {
            1.0
        } else {
            number("main_point", "confidence")
        },
        main_point,
    }
}

pub async fn judge(
    transcript: &str,
    listed_fillers: &[String],
    api_key: &str,
) -> Result<ContentJudgment, String> {
    let request = build_request(transcript, listed_fillers);
    let response = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?
        .post(ENDPOINT)
        .bearer_auth(api_key)
        .json(&request.body)
        .send()
        .await
        .map_err(|e| format!("TypeSafe request failed: {}", e.without_url()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 => "TypeSafe rejected the API key.".to_string(),
            429 | 529 => "TypeSafe is busy; try again in a moment.".to_string(),
            code => format!("TypeSafe returned HTTP {code}."),
        });
    }
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(parse_response(&request, &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "Outside the corner club, there's like this little I am trying to practice \
        my speech skills so that I have more confidence when I am presenting or facilitating some \
        of these conversations that I have at my work or just any other place I guess.";

    #[test]
    fn finds_ambiguous_words_by_token_position() {
        let tokens: Vec<&str> = REAL.split_whitespace().collect();
        let found = filler_candidates(&tokens, &HashSet::new());
        let phrases: Vec<&str> = found.iter().map(|c| c.2.as_str()).collect();

        assert_eq!(phrases, ["like", "so", "just", "i guess"]);
        let (first, last, _) = &found[3];
        assert_eq!(normalize(tokens[*first]), "i");
        assert_eq!(normalize(tokens[*last]), "guess"); // punctuation on "guess." ignored
    }

    #[test]
    fn words_on_the_users_own_list_are_not_asked_about() {
        let tokens: Vec<&str> = REAL.split_whitespace().collect();
        let listed = HashSet::from(["like".to_string()]);
        assert!(filler_candidates(&tokens, &listed)
            .iter()
            .all(|c| c.2 != "like"));
    }

    #[test]
    fn a_run_on_take_is_cut_into_clause_sized_units() {
        let units = clauses(REAL);
        assert!(units.len() >= 3, "{units:?}");
        assert!(units
            .iter()
            .all(|u| u.split_whitespace().count() <= CLAUSE_WORDS));
        assert_eq!(
            units.join(" "),
            REAL.split_whitespace().collect::<Vec<_>>().join(" ")
        );
    }

    /// Answers shaped like the ones the live API returned for this transcript.
    #[test]
    fn counts_only_confident_fillers_and_hides_uncertain_verdicts() {
        let request = build_request(REAL, &[]);
        let response = json!({ "answers": {
            "filler_C00": { "type": "noul", "noul": 0.92 },
            "filler_C01": { "type": "noul", "noul": 0.04 },
            "filler_C02": { "type": "noul", "noul": 0.50 },
            "filler_C03": { "type": "noul", "noul": 0.95 },
            "has_main_point": { "type": "noul", "noul": 0.64 },
            "main_point": { "type": "choice", "choice": "S01", "confidence": 0.10 },
            "opening": { "type": "score", "score": 1.19, "confidence": 0.0 },
            "closing": { "type": "score", "score": 0.01, "confidence": 0.99 },
            "specificity": { "type": "score", "score": 0.85, "confidence": 0.62 }
        }});
        let judgment = parse_response(&request, &response);

        let counted: Vec<&str> = judgment
            .counted_fillers()
            .map(|s| s.phrase.as_str())
            .collect();
        assert_eq!(counted, ["like", "i guess"]); // not "so that", not the 0.50 "just"

        assert!(judgment.main_point.is_some());
        assert_eq!(judgment.confident_main_point(), None); // 0.10 is a guess; show nothing

        let findings = judgment.findings();
        let metrics: Vec<&str> = findings.iter().map(|f| f.metric.as_str()).collect();
        assert_eq!(metrics, ["point", "specificity", "closing"]); // opening (0.0) hidden
        assert_eq!(findings[2].level, Level::WorkOn);
    }

    #[test]
    fn older_sessions_without_content_fields_still_decode() {
        let judgment: ContentJudgment = serde_json::from_str("{}").unwrap();
        assert_eq!(judgment, ContentJudgment::default());
    }
}
