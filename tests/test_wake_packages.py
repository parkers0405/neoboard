"""Exercise release payload assembly with distinct inert files, never an installer."""
import os
from pathlib import Path
import plistlib
import runpy
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile

if sys.version_info < (3, 11):
    raise unittest.SkipTest('release packager requires Python 3.11+')
from contextlib import chdir

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = runpy.run_path(str(ROOT / 'scripts/package-release.py'))['package']
NAMES = ('neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share', 'neochat-desktop')


class WakePackages(unittest.TestCase):
    def test_native_payloads(self):
        for os_name, asset in [('Darwin', 'darwin-aarch64'), ('Linux', 'linux-x86_64'), ('Windows', 'windows-x86_64')]:
            if os.name == 'nt' and os_name != 'Windows':
                continue  # Windows filesystems may lack POSIX permissions / unprivileged symlinks.
            with self.subTest(asset=asset), tempfile.TemporaryDirectory() as work, chdir(work):
                suffix = '.exe' if os_name == 'Windows' else ''
                for name in NAMES:
                    dest = Path('desktop' if name.endswith('-desktop') else '.') / 'target/release' / (name + suffix)
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_bytes(name.encode())
                    dest.chmod(0o755)
                for source in ('Cargo.toml', 'desktop/Cargo.toml', 'LICENSE', 'MCP.md', 'rune/LICENSE',
                               'vendor/winit-compat/winit-0.30.13/LICENSE', 'desktop/assets/fonts/OFL.GeistMono.txt',
                               'desktop/ai.neoism.neochat.desktop', 'desktop/LINUX-LAUNCHER.md', 'design/NeoBoard.png', 'design/NeoBoard.icns'):
                    dest = Path(source)
                    dest.parent.mkdir(exist_ok=True, parents=True)
                    dest.write_bytes((ROOT / source).read_bytes())

                def mac_tool(cmd, *, check):
                    if cmd[:2] == ['hdiutil', 'create']:
                        contents = Path(cmd[cmd.index('-srcfolder') + 1]) / 'NeoBoard.app/Contents'
                        entry = plistlib.loads((contents / 'Info.plist').read_bytes())['CFBundleExecutable']
                        self.assertEqual(entry, 'neochat-desktop')
                        for name in ('neoboard', entry):
                            self.assertEqual((contents / 'MacOS' / name).read_bytes(), name.encode())
                        Path(cmd[-1]).touch()  # inert stand-in for the disk image
                    else:
                        self.assertIn(cmd[0], ('codesign', 'hdiutil'))

                with patch.dict(os.environ, {}, clear=True), patch.dict(PACKAGE.__globals__, IDENTITY='-', NOTARY_PROFILE=None), \
                        patch('platform.system', return_value=os_name), patch('platform.machine', return_value=asset.split('-')[1]), \
                        patch('subprocess.run', side_effect=mac_tool):
                    PACKAGE(asset)

                archive = Path('dist', f'neoboard-{asset}.zip')
                with zipfile.ZipFile(archive) as z:
                    for name in ('neochat-desktop', 'neoboard'):
                        self.assertEqual(z.read(name + suffix), name.encode())
                if os_name == 'Windows':
                    wix = ET.parse(ROOT / 'scripts/neoboard.wxs')
                    group = wix.find(".//{*}ComponentGroup[@Directory='INSTALLFOLDER']")
                    self.assertIsNotNone(group)
                    names = {f.get('Name') for f in group.findall('./{*}Component/{*}File')}
                    self.assertTrue({'neochat-desktop.exe', 'neoboard.exe'} <= names)
                else:
                    prefix = 'NeoBoard.app/Contents/MacOS' if os_name == 'Darwin' else 'neoboard'
                    with tarfile.open(archive.with_suffix('.tar.gz')) as tar:
                        for name in ('neochat-desktop', 'neoboard'):
                            item = tar.getmember(f'{prefix}/{name}')
                            self.assertTrue(item.isfile() and item.mode & 0o111)
                            self.assertEqual(tar.extractfile(item).read(), name.encode())


if __name__ == '__main__':
    unittest.main()