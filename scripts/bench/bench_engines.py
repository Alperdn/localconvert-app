"""DEV-TIME ONLY. Builds one engine directory per benchmarked model under <bench_root>/engines/<label>/.

Every directory has the SAME whisper.cpp binary + app-local runtime (copied from the staged
production engine) and a manifest whose `model` block is patched for that model, so the
benchmark exercises exactly the production code path (manifest verification, ASCII model
exposure, real pipeline). The production manifest/model is NOT modified.

usage: bench_engines.py <bench_root>
"""
import hashlib, json, os, shutil, sys

root = sys.argv[1]
repo = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
prod = os.path.join(repo, "src-tauri", "engines", "speech")
REV = "5359861c739e955e79d9a303bcbc70fb988958b1"
MODELS = {
    "small-q5_1": ("ggml-small-q5_1", os.path.join(prod, "models", "ggml-small-q5_1.bin")),
    "medium-q5_0": ("ggml-medium-q5_0", os.path.join(root, "models", "ggml-medium-q5_0.bin")),
    "large-v3-turbo-q5_0": ("ggml-large-v3-turbo-q5_0", os.path.join(root, "models", "ggml-large-v3-turbo-q5_0.bin")),
}


def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


base = json.load(open(os.path.join(prod, "manifest.json"), encoding="utf-8-sig"))
for label, (mid, path) in MODELS.items():
    d = os.path.join(root, "engines", label)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(os.path.join(d, "models"))
    shutil.copytree(os.path.join(prod, "bin"), os.path.join(d, "bin"))
    fname = os.path.basename(path)
    try:
        os.link(path, os.path.join(d, "models", fname))
    except OSError:
        shutil.copy(path, os.path.join(d, "models", fname))
    m = json.loads(json.dumps(base))
    m["model"].update({
        "id": mid, "file_relative_path": "models/" + fname, "sha256": sha256(path),
        "size_bytes": os.path.getsize(path), "source_revision": REV,
        "source": f"https://huggingface.co/ggerganov/whisper.cpp/resolve/{REV}/{fname}",
    })
    json.dump(m, open(os.path.join(d, "manifest.json"), "w", encoding="utf-8"), indent=2)
    print(label, m["model"]["size_bytes"], m["model"]["sha256"][:16])
