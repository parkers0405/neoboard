"""Offline regression tests for the Windows MSI payload check."""
from pathlib import Path
import runpy
import struct
import unittest
import tempfile


check = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'scripts/smoke-release.py'))['check_windows_subsystems']


def image(subsystem, magic=0x20b):
    pe = 0x80
    header = bytearray(pe + 24 + 0xf0)
    header[:2] = b'MZ'
    struct.pack_into('<I', header, 0x3c, pe)
    header[pe:pe + 4] = b'PE\0\0'
    struct.pack_into('<H', header, pe + 20, 0xf0)
    struct.pack_into('<H', header, pe + 24, magic)
    struct.pack_into('<H', header, pe + 24 + 68, subsystem)
    return header


class WindowsSubsystems(unittest.TestCase):
    def check_images(self, **replacements):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            for name in ('neochat-desktop', 'neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share'):
                (folder / (name + '.exe')).write_bytes(replacements.get(name, image(2 if name == 'neochat-desktop' else 3)))
            check(folder)

    def test_pe32_and_pe32_plus(self):
        self.check_images(neochat=image(3, 0x10b))

    def test_console_desktop_or_gui_tools_rejected(self):
        for name in ('neochat-desktop', 'neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share'):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, f'{name}: expected PE subsystem'):
                self.check_images(**{name: image(3 if name == 'neochat-desktop' else 2)})

    def test_invalid_and_partial_headers_rejected(self):
        for data in (image(3, 0), image(3)[:0x80 + 24 + 69]):
            with self.assertRaisesRegex(ValueError, 'neochat: '):
                self.check_images(neochat=data)