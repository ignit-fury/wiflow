#!/usr/bin/env python3
"""Generate a 60-second macOS TTS fixture without using the microphone."""
import argparse
import struct
import subprocess
import tempfile
import wave
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
args = parser.parse_args()
audio = bytearray(60 * 16000 * 2)
clips = [
    (2, "The morning meeting starts at nine. Please bring the project notes."),
    (33, "The afternoon appointment is at three. We will review the latest changes."),
    (51, "The evening dinner begins at seven. This is the final sentence of the recording."),
]
with tempfile.TemporaryDirectory(prefix="wiflow-speech-") as directory:
    root = Path(directory)
    for index, (start, text) in enumerate(clips):
        aiff = root / f"{index}.aiff"
        wav = root / f"{index}.wav"
        subprocess.run(["say", "-v", "Samantha", "-o", str(aiff), text], check=True)
        subprocess.run(
            ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)],
            check=True,
        )
        with wave.open(str(wav), "rb") as clip:
            data = clip.readframes(clip.getnframes())
        if len(data) <= 32000 or max(abs(v[0]) for v in struct.iter_unpack("<h", data)) <= 1000:
            raise RuntimeError("Speech synthesis produced empty audio; check macOS voice access.")
        offset = start * 16000 * 2
        if offset + len(data) > len(audio):
            raise RuntimeError("Speech clip exceeds the 60-second fixture.")
        audio[offset:offset + len(data)] = data
with wave.open(str(args.output), "wb") as output:
    output.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
    output.writeframes(audio)
print(f"Wrote 60 seconds of 16 kHz mono speech to {args.output}")
