"""DEV-TIME ONLY. Peak RAM and CPU utilisation of whisper-cli per model, on the same 60 s slice, with
exactly the flags the app uses (threads = cores-1 clamped to 1..8, below-normal priority).

usage: bench_resources.py <bench_root>
"""
import os, shutil, subprocess, sys, tempfile, time
import numpy as np
import psutil
import soundfile as sf

root = sys.argv[1]
cores = os.cpu_count() or 4
threads = max(1, min(cores - 1, 8))
x, sr = sf.read(os.path.join(root, "dataset", "long_10min.wav"), dtype="int16")
work = tempfile.mkdtemp(prefix="benchres_")
sf.write(os.path.join(work, "clip.wav"), x[: 60 * sr], sr, subtype="PCM_16")
print(f"cores={cores} threads={threads}")
print("| Model | Peak RAM (working set) | Avg CPU (% of all cores) | Wall (60 s audio) | RTF |")
print("|---|---|---|---|---|")
for label in ("small-q5_1", "medium-q5_0", "large-v3-turbo-q5_0"):
    eng = os.path.join(root, "engines", label)
    if not os.path.isdir(eng):
        continue
    exe = os.path.join(eng, "bin", "whisper-cli.exe")
    model = [f for f in os.listdir(os.path.join(eng, "models"))][0]
    args = [exe, "-m", os.path.join(eng, "models", model), "-f", "clip.wav", "-l", "tr", "-t", str(threads),
            "-oj", "-np", "-pp", "-ng", "-sns", "-nth", "0.6", "-of", "out"]
    t0 = time.time()
    p = subprocess.Popen(args, cwd=work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=0x08004000)
    ps = psutil.Process(p.pid)
    ps.cpu_percent(None)
    peak, samples = 0, []
    while p.poll() is None:
        try:
            mem = ps.memory_info()
            peak = max(peak, getattr(mem, "peak_wset", mem.rss))
            samples.append(ps.cpu_percent(None))
        except psutil.Error:
            break
        time.sleep(0.2)
    wall = time.time() - t0
    avg = sum(samples) / max(len(samples), 1) / cores
    print(f"| {label} | {peak/1048576:.0f} MiB | {avg:.0f}% | {wall:.1f} s | {wall/60:.2f}x |")
shutil.rmtree(work, ignore_errors=True)
