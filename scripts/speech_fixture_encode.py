"""DEV-TIME ONLY: encodes the synthetic test fixtures for Ses Dikte.

Uses PyAV (which bundles an encoder-capable FFmpeg) purely to PRODUCE test
inputs. The shipped, minimal FFmpeg has no encoders on purpose; encoding the
fixtures with an independent implementation makes the shipped decoder's tests
meaningful. Invoked by generate-speech-fixtures.ps1.

usage: speech_fixture_encode.py <out_dir> <tts_raw.wav> <tts_chunk.wav> <long_minutes>
"""
import math, os, struct, sys, wave
import av
from av.audio.resampler import AudioResampler

out, raw, chunk, long_minutes = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
P = lambda n: os.path.join(out, n)


def transcode(src, dst, codec, *, fmt=None, rate=None, layout=None, options=None, container=None):
    with av.open(src) as i:
        ain = i.streams.audio[0]
        with av.open(dst, "w", format=container) as o:
            s = o.add_stream(codec, rate=rate or ain.rate)
            if options:
                s.options = options
            s.layout = layout or ("mono" if len(ain.layout.channels) == 1 else "stereo")
            if fmt:
                s.format = fmt
            rs = AudioResampler(format=s.format.name, layout=s.layout.name, rate=s.rate)
            def push(frames):
                for f in frames:
                    for p in s.encode(f):
                        o.mux(p)
            for fr in i.decode(ain):
                push(rs.resample(fr))
            push(rs.resample(None))
            for p in s.encode(None):
                o.mux(p)


def write_wav(path, samples_iter, rate=44100, channels=2):
    with wave.open(path, "wb") as w:
        w.setnchannels(channels); w.setsampwidth(2); w.setframerate(rate)
        w.writeframes(b"".join(samples_iter))


# --- Turkish speech in every accepted container ---------------------------------
transcode(raw, P("tr_speech.wav"), "pcm_s16le", rate=44100, layout="stereo", container="wav")   # NON-normalized on purpose
transcode(raw, P("tr_speech.mp3"), "libmp3lame", fmt="s16p", container="mp3")
transcode(raw, P("tr_speech.m4a"), "aac", fmt="fltp", options={"b": "96000"}, container="mp4")
transcode(raw, P("tr_speech.aac"), "aac", fmt="fltp", options={"b": "96000"}, container="adts")
transcode(raw, P("tr_speech.flac"), "flac", fmt="s16", container="flac")
import soundfile as sf   # libsndfile ships a real libvorbis (PyAV only has the experimental native one)
_d, _sr = sf.read(raw)
sf.write(P("tr_speech.ogg"), _d, _sr, format="OGG", subtype="VORBIS")                                   # Ogg/Vorbis
transcode(raw, P("tr_speech_opus.ogg"), "libopus", fmt="flt", rate=48000, container="ogg")                        # Ogg/Opus

# --- Non-speech ------------------------------------------------------------------
rate = 44100
write_wav(P("silence.wav"), (struct.pack("<hh", 0, 0) for _ in range(rate * 10)), rate)
def music():
    chords = [(261.63, 329.63, 392.0), (220.0, 261.63, 329.63), (174.61, 220.0, 261.63), (196.0, 246.94, 293.66)]
    for n in range(rate * 12):
        t = n / rate
        ch = chords[int(t // 3) % len(chords)]
        env = 0.5 * (1 - math.cos(2 * math.pi * ((t % 3) / 3)))
        v = sum(math.sin(2 * math.pi * f * t) for f in ch) / 3 * env * 0.35
        s = int(v * 32767)
        yield struct.pack("<hh", s, s)
write_wav(P("music_like.wav"), music(), rate)

# Valid MP4-family container with a VIDEO stream and no audio (named .m4a to pass the extension gate).
with av.open(P("no_audio_stream.m4a"), "w", format="mp4") as o:
    v = o.add_stream("mpeg4", rate=10); v.width = 64; v.height = 64; v.pix_fmt = "yuv420p"
    for _ in range(10):
        for p in v.encode(av.VideoFrame(64, 64, "yuv420p")):
            o.mux(p)
    for p in v.encode(None):
        o.mux(p)

# --- Long Turkish speech (16 kHz mono) -------------------------------------------
tmp16 = P("_chunk16.wav")
transcode(chunk, tmp16, "pcm_s16le", rate=16000, layout="mono", container="wav")
with wave.open(tmp16, "rb") as w:
    frames, nch, sw, fr = w.readframes(w.getnframes()), w.getnchannels(), w.getsampwidth(), w.getframerate()
secs = len(frames) / (nch * sw * fr)
reps = math.ceil(long_minutes * 60 / secs)
with wave.open(P("tr_speech_long.wav"), "wb") as w:
    w.setnchannels(nch); w.setsampwidth(sw); w.setframerate(fr)
    for _ in range(reps):
        w.writeframes(frames)
os.remove(tmp16)
print("fixtures written:", sorted(os.listdir(out)))
