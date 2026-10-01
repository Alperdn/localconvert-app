"""DEV-TIME ONLY. Builds the Turkish benchmark dataset used to compare Whisper models.

Real human speech comes from Google FLEURS (tr_tr, CC-BY-4.0), downloaded to a TEMP folder by
the caller; nothing from it is ever committed. Derived conditions (noise, quiet speech, long
concatenation) are generated from those same clips. Non-speech conditions are synthetic
(silence, room noise, hiss) plus, when reachable, a public-domain instrumental recording from
Wikimedia Commons.

usage: bench_prepare.py <fleurs_test.parquet> <out_dir>
Output: <out_dir>/*.wav (16 kHz mono PCM16) and <out_dir>/manifest.tsv  (file \\t category \\t reference)
"""
import io, json, math, os, random, re, struct, sys, urllib.parse, urllib.request
import numpy as np
import pyarrow.parquet as pq
import soundfile as sf

parquet, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
rng = random.Random(20260921)
nrng = np.random.default_rng(20260921)
SR = 16000
rows = []  # (file, category, reference)


def save(name, x, category, ref=""):
    x = np.clip(x, -1.0, 1.0)
    sf.write(os.path.join(out, name), (x * 32767).astype(np.int16), SR, subtype="PCM_16")
    rows.append((name, category, ref.replace("\t", " ").replace("\n", " ")))


def rms_db(x):
    return 20 * math.log10(max(float(np.sqrt(np.mean(x ** 2))), 1e-9))


def pink(n):
    w = nrng.standard_normal(n)
    f = np.fft.rfft(w)
    f /= np.sqrt(np.maximum(np.arange(len(f)), 1))
    p = np.fft.irfft(f, n)
    return p / np.sqrt(np.mean(p ** 2))


def median_f0(x, sr=SR):
    """Median fundamental frequency (Hz) over voiced frames, by autocorrelation. Heuristic speaker-sex
    labelling: FLEURS' own `gender` column is 0 for every Turkish row, so it cannot be used."""
    frame, hop = int(0.04 * sr), int(0.02 * sr)
    lo, hi = int(sr / 400), int(sr / 70)
    e_thr = np.percentile([np.sqrt(np.mean(x[i:i + frame] ** 2)) for i in range(0, len(x) - frame, hop)], 60)
    f0s = []
    for i in range(0, len(x) - frame, hop):
        f = x[i:i + frame] - np.mean(x[i:i + frame])
        if np.sqrt(np.mean(f ** 2)) < e_thr:
            continue
        ac = np.correlate(f, f, mode="full")[frame - 1:]
        if ac[0] <= 0:
            continue
        seg = ac[lo:hi]
        k = int(np.argmax(seg)) + lo
        if ac[k] / ac[0] > 0.5:
            f0s.append(sr / k)
    return float(np.median(f0s)) if len(f0s) >= 10 else float("nan")


pf = pq.ParquetFile(parquet)
tbl = pf.read(columns=["num_samples", "audio", "raw_transcription", "gender"]).to_pylist()
clips = []
for r in tbl:
    x, sr = sf.read(io.BytesIO(r["audio"]["bytes"]), dtype="float32")
    if x.ndim > 1:
        x = x.mean(axis=1)
    assert sr == SR, sr
    dur = len(x) / SR
    clips.append({"x": x, "dur": dur, "ref": r["raw_transcription"], "cps": len(r["raw_transcription"]) / dur})
for c in clips:
    c["f0"] = median_f0(c["x"])
    c["g"] = 1 if c["f0"] >= 165 else 0        # >= 165 Hz -> female-range voice (heuristic)
f0s = np.array([c["f0"] for c in clips if not math.isnan(c["f0"])])
print("f0 percentiles 5/25/50/75/95:", np.percentile(f0s, [5, 25, 50, 75, 95]).round(0))
print("clips:", len(clips), "male-range", sum(c["g"] == 0 for c in clips), "female-range", sum(c["g"] == 1 for c in clips))

ok = [i for i, c in enumerate(clips) if 6.0 <= c["dur"] <= 16.0]
used = set()


def pick(pool, n):
    pool = [i for i in pool if i not in used]
    rng.shuffle(pool)
    got = pool[:n]
    used.update(got)
    return got


# fast speech first (the top of the chars/sec ranking, either gender)
fast = sorted(ok, key=lambda i: -clips[i]["cps"])[:8]
used.update(fast)
male = pick([i for i in ok if clips[i]["g"] == 0], 15)
female = pick([i for i in ok if clips[i]["g"] == 1], 15)
for i in male:
    save(f"male_{i:03d}.wav", clips[i]["x"], "male", clips[i]["ref"])
for i in female:
    save(f"female_{i:03d}.wav", clips[i]["x"], "female", clips[i]["ref"])
for i in fast:
    save(f"fast_{i:03d}.wav", clips[i]["x"], "fast", clips[i]["ref"])

# mild background noise: pink noise at SNR 15 dB over 4 male + 4 female clips (reused speech, new condition)
for i in male[:4] + female[:4]:
    x = clips[i]["x"]
    n = pink(len(x)) * (10 ** ((rms_db(x) - 15) / 20))
    save(f"noise15_{i:03d}.wav", x + n.astype(np.float32), "noise15", clips[i]["ref"])
# quiet speech: attenuated so overall RMS is about -40 dBFS (probes false 'no speech' at the energy gate)
for i in male[4:6] + female[4:6]:
    x = clips[i]["x"]
    g = 10 ** ((-40 - rms_db(x)) / 20)
    save(f"quiet40_{i:03d}.wav", x * g, "quiet40", clips[i]["ref"])

# long recording: the remaining clips concatenated with 0.4 s pauses until >= 10.5 minutes
rest = [i for i in range(len(clips)) if i not in used]
rng.shuffle(rest)
pause = np.zeros(int(0.4 * SR), dtype=np.float32)
parts, refs, total = [], [], 0.0
for i in rest:
    parts += [clips[i]["x"], pause]
    refs.append(clips[i]["ref"])
    total += clips[i]["dur"] + 0.4
    if total >= 630:
        break
save("long_10min.wav", np.concatenate(parts), "long", " ".join(refs))
print(f"long recording: {total/60:.1f} min from {len(refs)} clips")

# non-speech
save("silence_30s.wav", np.zeros(30 * SR, dtype=np.float32), "silence")
save("room_noise_30s.wav", (pink(30 * SR) * 10 ** (-62 / 20)).astype(np.float32), "room_noise")
save("hiss_30s.wav", (pink(30 * SR) * 10 ** (-32 / 20)).astype(np.float32), "hiss")
synth = os.path.join(os.path.dirname(__file__), "..", "..", "src-tauri", "tests", "fixtures", "speech", "generated", "music_like.wav")
if os.path.isfile(synth):
    x, sr = sf.read(synth, dtype="float32")
    x = x.mean(axis=1) if x.ndim > 1 else x
    idx = np.linspace(0, len(x) - 1, int(len(x) * SR / sr)).astype(int)
    save("music_synth.wav", x[idx], "music_synth")

# real instrumental music (public domain) from Wikimedia Commons, best effort
try:
    q = urllib.parse.urlencode({"action": "query", "list": "search", "srnamespace": 6, "srlimit": 8, "format": "json",
                                "srsearch": "filetype:audio piano instrumental public domain ogg"})
    req = urllib.request.Request("https://commons.wikimedia.org/w/api.php?" + q, headers={"User-Agent": "MEB-bench/1.0 (dev benchmark)"})
    hits = json.load(urllib.request.urlopen(req, timeout=30))["query"]["search"]
    got = 0
    for h in hits:
        t = urllib.parse.urlencode({"action": "query", "titles": h["title"], "prop": "imageinfo", "iiprop": "url|size|mime", "format": "json"})
        r2 = urllib.request.Request("https://commons.wikimedia.org/w/api.php?" + t, headers={"User-Agent": "MEB-bench/1.0 (dev benchmark)"})
        info = list(json.load(urllib.request.urlopen(r2, timeout=30))["query"]["pages"].values())[0]["imageinfo"][0]
        if info["size"] > 12_000_000 or info["mime"] not in ("audio/ogg", "application/ogg", "audio/flac", "audio/wav", "audio/x-wav"):
            continue
        data = urllib.request.urlopen(urllib.request.Request(info["url"], headers={"User-Agent": "MEB-bench/1.0 (dev benchmark)"}), timeout=90).read()
        x, sr = sf.read(io.BytesIO(data), dtype="float32")
        x = x.mean(axis=1) if x.ndim > 1 else x
        seg = x[int(15 * sr): int(15 * sr) + int(45 * sr)]
        if len(seg) < 20 * sr:
            continue
        idx = np.linspace(0, len(seg) - 1, int(len(seg) * SR / sr)).astype(int)
        save("music_real.wav", seg[idx], "music_real")
        open(os.path.join(out, "music_real.source.txt"), "w", encoding="utf-8").write(h["title"] + "\n" + info["url"] + "\n")
        print("real music:", h["title"])
        got = 1
        break
    if not got:
        print("no suitable real-music file found on Commons")
except Exception as e:
    print("real music unavailable:", e)

with open(os.path.join(out, "manifest.tsv"), "w", encoding="utf-8", newline="\n") as f:
    for r in rows:
        f.write("\t".join(r) + "\n")
tot = {}
for r in rows:
    tot[r[1]] = tot.get(r[1], 0) + 1
print("dataset:", tot, "files:", len(rows))
