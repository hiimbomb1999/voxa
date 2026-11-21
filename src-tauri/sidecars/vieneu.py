"""VieNeu-TTS over pipes. stdout is reserved for PCM frames and control lines."""

import base64
import contextlib
import json
import sys

protocol = sys.stdout

with contextlib.redirect_stdout(sys.stderr):
    import numpy as np
    import soxr
    from vieneu import Vieneu

    engine = Vieneu(backend="onnx", precision="fp32")
    voices = engine.list_preset_voices()


def pcm_frames(text, voice):
    resampler = soxr.ResampleStream(engine.sample_rate, 24_000, 1)
    for audio in engine.infer_stream(text, voice=voice):
        yield resampler.resample_chunk(audio)
    yield resampler.resample_chunk(np.empty(0, dtype=np.float32), last=True)


def send(message):
    print(message, file=protocol, flush=True)


if sys.argv[1:] == ["--install"]:
    with contextlib.redirect_stdout(sys.stderr):
        # Warm the phonemizer and verify that both translation targets work.
        for text in ("Xin chào, đây là Voxa.", "Hello, this is Voxa."):
            assert sum(len(frame) for frame in pcm_frames(text, "Hải Đăng")) > 0
    send(json.dumps(voices, ensure_ascii=False))
else:
    send("ready")
    for line in sys.stdin:
        try:
            text, voice = json.loads(line)
            with contextlib.redirect_stdout(sys.stderr):
                for frame in pcm_frames(text, voice):
                    if len(frame):
                        pcm = (np.clip(frame, -1, 1) * 32767).astype("<i2")
                        send(base64.b64encode(pcm).decode("ascii"))
            send("")
        except Exception:
            send("error")
