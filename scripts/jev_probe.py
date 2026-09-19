#!/usr/bin/env python3
"""Probe: can Jev (TypeSafe) judge what the word list and the free-text coach can't?

Runs three kinds of judgment over one practice transcript, in ONE request:
  1. Context-dependent fillers — code finds candidates ("like", "so", "I guess"…),
     Jev says whether each occurrence is a filler here. Code does the counting
     (Jev does not count reliably).
  2. Content scores — five independent dimensions with concrete levels.
  3. Main point — which sentence states it (code then quotes that sentence, so the
     quote can never be invented), plus whether there is one at all.

Usage:
  export TYPESAFE_API_KEY=...            # never commit this
  scripts/jev_probe.py                   # latest session from the dev app's database
  scripts/jev_probe.py "some transcript" # or any text

Stdlib only. Nothing is written anywhere; this only prints.
"""
import json, os, re, sqlite3, sys, time, urllib.request

DB = os.path.expanduser("~/Library/Application Support/com.stalinzbb.voix/history.db")
AMBIGUOUS = ["you know", "i mean", "kind of", "sort of", "i guess", "like", "so", "actually",
             "basically", "literally", "right", "well", "just", "okay"]
MAX_CANDIDATES = 40


def transcript():
    if len(sys.argv) > 1:
        return sys.argv[1]
    row = sqlite3.connect(DB).execute(
        "SELECT transcription_text FROM transcription_history "
        "WHERE practice_json IS NOT NULL ORDER BY id DESC LIMIT 1").fetchone()
    if not row:
        sys.exit("no practice session found; pass a transcript as an argument")
    return row[0]


def sentences(text):
    """Sentence-ish units. ASR often returns one run-on, so long ones are chunked."""
    out = []
    for s in re.split(r"(?<=[.!?])\s+", text.strip()):
        words = s.split()
        while len(words) > 30:
            out.append(" ".join(words[:20])); words = words[20:]
        if words:
            out.append(" ".join(words))
    return out


def filler_candidates(text):
    found = []
    pattern = r"\b(" + "|".join(re.escape(p) for p in AMBIGUOUS) + r")\b"
    for m in re.finditer(pattern, text, flags=re.I):
        lo, hi = max(0, m.start() - 60), min(len(text), m.end() + 60)
        context = text[lo:m.start()] + "[[" + m.group(0) + "]]" + text[m.end():hi]
        found.append({"phrase": m.group(0).lower(), "context": context.strip()})
    return found[:MAX_CANDIDATES]


def score(instructions, levels):
    return {"type": "score", "instructions": instructions, "criteria": levels}


def build(text):
    sents = sentences(text)
    cands = filler_candidates(text)
    state = {
        "transcript": text,
        "sentences": {f"S{i:02d}": s for i, s in enumerate(sents)},
        "candidates": {f"C{i:02d}": c for i, c in enumerate(cands)},
    }
    q = {}
    for cid in state["candidates"]:
        q[f"filler_{cid}"] = {
            "type": "noul",
            "instructions": f"In `candidates.{cid}.context`, the phrase inside [[ ]] was spoken aloud. "
                            "Is it a filler or verbal hedge in this sentence?",
            "criteria": {
                "true": "It carries no meaning here: deleting it leaves the sentence saying the same "
                        "thing (e.g. 'there's [[like]] this little shop', 'or any other place [[I guess]]').",
                "false": "It does real work here: a verb, comparison, connective that links cause and "
                         "result, or an adverb that changes the meaning (e.g. 'I [[like]] this', "
                         "'it looks [[like]] rain', 'it was late [[so]] we left').",
            },
        }
    q["has_main_point"] = {
        "type": "noul",
        "instructions": "Does `transcript` contain a sentence that states what the speaker wants the "
                        "listener to know, believe or do?",
    }
    q["main_point_sentence"] = {
        "type": "choice",
        "instructions": "Which entry of `sentences` best states the speaker's main point?",
        "criteria": {sid: None for sid in state["sentences"]},
    }
    q["opening"] = score("How does the FIRST entry of `sentences` open the talk?", [
        "Starts mid-thought, off topic, or with throat-clearing unrelated to the point",
        "Greets or introduces the topic in general terms without saying why it matters",
        "States the topic and why the listener should care",
        "Opens with the point itself, or a concrete hook that leads straight into it",
    ])
    q["structure"] = score("How easy is it to follow the order of ideas in `transcript`?", [
        "Ideas arrive in no discernible order; thoughts are abandoned mid-sentence",
        "There is a rough thread, but it wanders or restarts",
        "A clear sequence of ideas, though the transitions are not signposted",
        "A clear sequence with explicit signposts (first, second, so, in short)",
    ])
    q["specificity"] = score("How concrete is what the speaker says in `transcript`?", [
        "Only general statements; no example, number, name or concrete detail",
        "Mostly general, with one concrete detail",
        "Several concrete details, examples or numbers support the claims",
        "Every main claim is backed by a concrete example, number or named case",
    ])
    q["closing"] = score("How does `transcript` end?", [
        "Trails off, or ends on a hedge such as 'I guess' or 'or whatever'",
        "Stops without a conclusion",
        "Ends by restating the point",
        "Ends with a clear ask, decision or next step for the listener",
    ])
    q["conviction"] = score("How committed does the speaker sound to their own statements in `transcript`?", [
        "Most statements are hedged or qualified (maybe, I guess, kind of, I think)",
        "Frequent hedging alongside some plain statements",
        "Mostly plain statements with occasional hedging",
        "States things plainly and owns them",
    ])
    return state, q, sents, cands


def main():
    key = os.environ.get("TYPESAFE_API_KEY") or sys.exit("set TYPESAFE_API_KEY first")
    text = transcript()
    state, questions, sents, cands = build(text)
    body = json.dumps({"model": "jev-latest", "state": state, "questions": questions}).encode()
    req = urllib.request.Request("https://api.typesafe.ai/v1/systemone", data=body, headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    started = time.time()
    with urllib.request.urlopen(req, timeout=120) as r:
        res = json.load(r)
    a = res["answers"]
    print(f"{len(questions)} questions in one request · {time.time() - started:.2f}s · usage {res.get('usage')}\n")

    print("FILLERS (word list would have counted none of these)")
    count = 0
    for i, c in enumerate(cands):
        p = a[f"filler_C{i:02d}"]["noul"]
        count += p >= 0.7
        print(f"  {p:4.2f} {'FILLER' if p >= 0.7 else 'unsure' if p > 0.35 else 'keep  '}  {c['context']}")
    print(f"  -> {count} contextual fillers (code counted; threshold 0.7 is a guess to tune)\n")

    print("MAIN POINT")
    mp = a["main_point_sentence"]
    print(f"  present: {a['has_main_point']['noul']:.2f}")
    print(f"  quoted : \"{state['sentences'][mp['choice']]}\"  (confidence {mp.get('confidence', 0):.2f})\n")

    print("CONTENT (0 = weakest level, 3 = strongest)")
    for k in ["opening", "structure", "specificity", "closing", "conviction"]:
        print(f"  {k:12s} {a[k]['score']:.2f}  confidence {a[k].get('confidence', 0):.2f}")


if __name__ == "__main__":
    main()
