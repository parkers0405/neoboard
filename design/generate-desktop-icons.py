"""Regenerate the checked-in PNG and ICO on macOS; not needed to build or run."""
from pathlib import Path
import struct
import subprocess
import tempfile


design = Path(__file__).resolve().parent
sizes = [16, 24, 32, 48, 64, 128, 256]
frames = []
with tempfile.TemporaryDirectory() as tmp:
    for size in sizes:
        png = Path(tmp) / f'{size}.png'
        subprocess.run(['sips', '-s', 'format', 'png', '-z', str(size), str(size),
                        design / 'neoboard-icon-preview.svg', '--out', png], check=True)
        frames.append(png.read_bytes())

(design / 'NeoBoard.png').write_bytes(frames[-1])
directory = bytearray(struct.pack('<HHH', 0, 1, len(frames)))
offset = 6 + 16 * len(frames)
for size, frame in zip(sizes, frames):
    directory.extend(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0,
                                 1, 32, len(frame), offset))
    offset += len(frame)
(design / 'NeoBoard.ico').write_bytes(directory + b''.join(frames))