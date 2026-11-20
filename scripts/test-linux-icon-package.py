"""Test the Linux archive layout without building native binaries or touching dist/."""
from contextlib import chdir
from pathlib import Path
import runpy
import tarfile
import tempfile
from unittest.mock import patch
import zipfile


root = Path(__file__).resolve().parents[1]
package = runpy.run_path(str(root / 'scripts/package-release.py'))['package']
app_id = 'ai.neoism.neochat'
with tempfile.TemporaryDirectory() as tmp, chdir(tmp):
    for source in ['Cargo.toml', 'desktop/Cargo.toml', 'LICENSE', 'MCP.md',
                   'rune/LICENSE', 'vendor/winit-compat/winit-0.30.13/LICENSE',
                   'desktop/assets/fonts/OFL.GeistMono.txt', 'desktop/LINUX-LAUNCHER.md',
                   f'desktop/{app_id}.desktop', 'design/NeoBoard.png']:
        dest = Path(source)
        dest.parent.mkdir(exist_ok=True, parents=True)
        dest.write_bytes((root / source).read_bytes())
    for name in ['neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share', 'neochat-desktop']:
        dest = Path('desktop' if name.endswith('-desktop') else '.') / 'target/release' / name
        dest.parent.mkdir(exist_ok=True, parents=True)
        dest.touch(mode=0o755)
    with patch('platform.system', return_value='Linux'), patch('platform.machine', return_value='x86_64'):
        package('linux-x86_64')
    with tarfile.open('dist/neoboard-linux-x86_64.tar.gz') as tar, zipfile.ZipFile('dist/neoboard-linux-x86_64.zip') as z:
        share = 'neoboard-desktop-integration'
        entry = tar.extractfile(f'{share}/applications/{app_id}.desktop').read().decode().splitlines()
        assert f'Icon={app_id}' in entry and f'StartupWMClass={app_id}' in entry
        assert 'Exec=neochat-desktop' in entry and tar.getmember('neoboard/neochat-desktop').mode & 0o111
        png = tar.extractfile(f'{share}/icons/hicolor/256x256/apps/{app_id}.png').read()
        assert png == Path('design/NeoBoard.png').read_bytes()
        assert sorted(n.removeprefix('neoboard/') for n in tar.getnames() if n.startswith('neoboard/')) == sorted(z.namelist())
print('Linux launcher, hicolor icon and portable layout verified')