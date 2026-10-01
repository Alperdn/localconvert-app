"""DEV-TIME ONLY. Scores the benchmark results written by the Rust runner
(`benchmark_model_when_configured`) and prints markdown tables.

usage: bench_analyze.py <bench_root>
"""
import json, os, re, sys, unicodedata
from collections import defaultdict

root = sys.argv[1]
SPEECH_CATS = ["male", "female", "fast", "noise15", "quiet40", "long"]
NONSPEECH_CATS = ["silence", "room_noise", "hiss", "music_synth", "music_real"]
SPECIAL = ["ı", "İ", "ş", "ğ", "ü", "ö", "ç"]


def tr_lower(s):
    return s.replace("İ", "i").replace("I", "ı").lower()


def strip_punct(s):
    return re.sub(r"[^\w\s']", " ", s).replace("'", "")


def words_strict(s):
    return strip_punct(tr_lower(s)).split()


def fold(w):
    for a, b in zip("ışğüöç", "isguoc"):
        w = w.replace(a, b)
    return w


def align(ref, hyp):
    """Levenshtein alignment on word lists -> (errors, list of (ref_idx -> hyp_idx or None))."""
    n, m = len(ref), len(hyp)
    d = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        d[i][0] = i
    for j in range(m + 1):
        d[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i][j] = min(d[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1]), d[i - 1][j] + 1, d[i][j - 1] + 1)
    i, j, pairs = n, m, {}
    while i > 0 or j > 0:
        if i > 0 and j > 0 and d[i][j] == d[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1]):
            pairs[i - 1] = j - 1
            i, j = i - 1, j - 1
        elif i > 0 and d[i][j] == d[i - 1][j] + 1:
            pairs[i - 1] = None
            i -= 1
        else:
            j -= 1
    return d[n][m], pairs


def load(label):
    p = os.path.join(root, "results", f"{label}.jsonl")
    return [json.loads(l) for l in open(p, encoding="utf-8")] if os.path.isfile(p) else []


refs = {}
for line in open(os.path.join(root, "dataset", "manifest.tsv"), encoding="utf-8"):
    c = line.rstrip("\n").split("\t")
    refs[c[0]] = c[2] if len(c) > 2 else ""

MODELS = ["small-q5_1", "medium-q5_0", "large-v3-turbo-q5_0"]
sizes = {m: json.load(open(os.path.join(root, "engines", m, "manifest.json"), encoding="utf-8"))["model"]["size_bytes"] for m in MODELS if os.path.isdir(os.path.join(root, "engines", m))}
summary = {}

for m in MODELS:
    rs = load(m)
    if not rs:
        continue
    S = defaultdict(lambda: {"ref": 0, "err": 0, "err_fold": 0, "audio": 0.0, "wall": 0.0, "n": 0, "rejected": 0})
    spec = {c: {"tot": 0, "ok": 0} for c in SPECIAL}
    punct = {"ref_comma": 0, "hyp_comma": 0, "ref_stop": 0, "hyp_stop": 0, "ref_q": 0, "hyp_q": 0, "hyp_cap_start": 0, "hyp_sent": 0}
    nonspeech = defaultdict(list)
    false_reject = []
    for r in rs:
        cat = r["category"]
        if cat in NONSPEECH_CATS:
            nonspeech[cat].append(r)
            continue
        ref_raw, hyp_raw = refs[r["file"]], r["text"]
        ref_w, hyp_w = words_strict(ref_raw), words_strict(hyp_raw)
        s = S[cat]
        s["n"] += 1
        s["audio"] += r["audio_s"]
        s["wall"] += r["wall_s"]
        s["ref"] += len(ref_w)
        if not r["ok"]:
            s["rejected"] += 1
            s["err"] += len(ref_w)
            s["err_fold"] += len(ref_w)
            false_reject.append((r["file"], r.get("code")))
            continue
        e, pairs = align(ref_w, hyp_w)
        s["err"] += e
        s["err_fold"] += align([fold(w) for w in ref_w], [fold(w) for w in hyp_w])[0]
        if cat in ("male", "female", "fast", "noise15", "quiet40", "long"):
            # Turkish character accuracy: a reference word containing the char counts as correct when its
            # aligned hypothesis word is identical (strict, incl. the char).
            for ci, w in enumerate(ref_w):
                for c in SPECIAL:
                    if c == "İ":
                        continue
                    if c in w:
                        spec[c]["tot"] += 1
                        j = pairs.get(ci)
                        if j is not None and hyp_w[j] == w:
                            spec[c]["ok"] += 1
            # capital dotted İ: case-sensitive words containing İ in the reference
            ref_cs = strip_punct(ref_raw).split()
            hyp_cs = strip_punct(hyp_raw).split()
            _, pcs = align([tr_lower(x) for x in ref_cs], [tr_lower(x) for x in hyp_cs])
            for ci, w in enumerate(ref_cs):
                if "İ" in w:
                    spec["İ"]["tot"] += 1
                    j = pcs.get(ci)
                    if j is not None and hyp_cs[j] == w:
                        spec["İ"]["ok"] += 1
            punct["ref_comma"] += ref_raw.count(",")
            punct["hyp_comma"] += hyp_raw.count(",")
            punct["ref_stop"] += len(re.findall(r"[.](?=\s|$)", ref_raw))
            punct["hyp_stop"] += len(re.findall(r"[.](?=\s|$)", hyp_raw))
            punct["ref_q"] += ref_raw.count("?")
            punct["hyp_q"] += hyp_raw.count("?")
            sents = [x.strip() for x in re.split(r"(?<=[.?!])\s+", hyp_raw.strip()) if x.strip()]
            punct["hyp_sent"] += len(sents)
            punct["hyp_cap_start"] += sum(1 for x in sents if x[:1].isupper())
    tot_ref = sum(S[c]["ref"] for c in SPEECH_CATS if c in S)
    tot_err = sum(S[c]["err"] for c in SPEECH_CATS if c in S)
    tot_fold = sum(S[c]["err_fold"] for c in SPEECH_CATS if c in S)
    tot_audio = sum(S[c]["audio"] for c in SPEECH_CATS if c in S)
    tot_wall = sum(S[c]["wall"] for c in SPEECH_CATS if c in S)
    summary[m] = dict(S=S, spec=spec, punct=punct, nonspeech=nonspeech, false_reject=false_reject,
                      wer=tot_err / max(tot_ref, 1), wer_fold=tot_fold / max(tot_ref, 1), rtf=tot_wall / max(tot_audio, 1e-9),
                      ref_words=tot_ref, audio=tot_audio, wall=tot_wall)

print("## Overall (all speech conditions, same files for every model)\n")
print("| Model | Size | WER strict | WER (diacritics folded) | Real-time factor | Speech audio | Wall time | Speech files rejected as no-speech |")
print("|---|---|---|---|---|---|---|---|")
for m, x in summary.items():
    rej = sum(x["S"][c]["rejected"] for c in SPEECH_CATS if c in x["S"])
    print(f"| {m} | {sizes[m]/1048576:.0f} MiB | {x['wer']*100:.1f}% | {x['wer_fold']*100:.1f}% | {x['rtf']:.2f}x | {x['audio']/60:.1f} min | {x['wall']/60:.1f} min | {rej} |")

print("\n## WER by condition (strict) / RTF\n")
print("| Condition | " + " | ".join(summary) + " |")
print("|---|" + "---|" * len(summary))
for c in SPEECH_CATS:
    cells = []
    for m, x in summary.items():
        s = x["S"].get(c)
        cells.append(f"{s['err']/max(s['ref'],1)*100:.1f}% / {s['wall']/max(s['audio'],1e-9):.2f}x (n={s['n']})" if s else "-")
    print(f"| {c} | " + " | ".join(cells) + " |")

print("\n## Turkish character accuracy (reference words containing the character reproduced exactly)\n")
print("| Char | " + " | ".join(summary) + " |")
print("|---|" + "---|" * len(summary))
for c in SPECIAL:
    cells = []
    for m, x in summary.items():
        s = x["spec"][c]
        cells.append(f"{s['ok']/s['tot']*100:.0f}% ({s['ok']}/{s['tot']})" if s["tot"] else "n/a")
    print(f"| {c} | " + " | ".join(cells) + " |")

print("\n## Punctuation / casing (hypothesis vs reference, speech files)\n")
print("| Metric | " + " | ".join(summary) + " |")
print("|---|" + "---|" * len(summary))
for label, a, b in [("commas (hyp/ref)", "hyp_comma", "ref_comma"), ("full stops (hyp/ref)", "hyp_stop", "ref_stop"), ("question marks (hyp/ref)", "hyp_q", "ref_q")]:
    print(f"| {label} | " + " | ".join(f"{x['punct'][a]}/{x['punct'][b]}" for x in summary.values()) + " |")
print("| sentences starting uppercase | " + " | ".join(f"{x['punct']['hyp_cap_start']}/{x['punct']['hyp_sent']}" for x in summary.values()) + " |")

print("\n## Non-speech behaviour (pipeline as shipped: energy gate + Whisper + filters)\n")
print("| Input | " + " | ".join(summary) + " |")
print("|---|" + "---|" * len(summary))
for c in NONSPEECH_CATS:
    cells = []
    for m, x in summary.items():
        rs = x["nonspeech"].get(c, [])
        if not rs:
            cells.append("-")
            continue
        r = rs[0]
        cells.append("rejected (no speech)" if not r["ok"] else f"TEXT: \"{r['text'][:60]}\" ({len(r['text'].split())} words)")
    print(f"| {c} | " + " | ".join(cells) + " |")

print("\n## False rejections of real speech\n")
for m, x in summary.items():
    print(f"- {m}: {x['false_reject'] or 'none'}")

json.dump({m: {"wer": x["wer"], "wer_fold": x["wer_fold"], "rtf": x["rtf"], "audio_s": x["audio"], "wall_s": x["wall"]} for m, x in summary.items()},
          open(os.path.join(root, "results", "summary.json"), "w"), indent=2)
