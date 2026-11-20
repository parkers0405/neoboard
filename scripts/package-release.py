"""Package native builds; run on the target machine, from the repo root."""
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile


def package(asset):
    system = {'Linux': 'linux', 'Darwin': 'darwin', 'Windows': 'windows'}[platform.system()]
    arch = {'amd64': 'x86_64', 'x86_64': 'x86_64', 'arm64': 'aarch64', 'aarch64': 'aarch64'}[platform.machine().lower()]
    if asset != f'{system}-{arch}':
        sys.exit('Asset name does not match native runner')
    version = tomllib.loads(Path('Cargo.toml').read_text())['package']['version']
    if tomllib.loads(Path('desktop/Cargo.toml').read_text())['package']['version'] != version:
        sys.exit('Desktop and CLI package versions must match')
    if os.getenv('GITHUB_REF_TYPE') == 'tag' and os.getenv('GITHUB_REF_NAME') != f'v{version}':
        sys.exit('Tag must match Cargo package version')
    suffix = '.exe' if system == 'windows' else ''
    files = [Path('target/release') / (name + suffix) for name in ('neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share')]
    files += [Path('desktop/target/release') / ('neochat-desktop' + suffix), Path('LICENSE'), Path('MCP.md'), Path('desktop/assets/fonts/OFL.GeistMono.txt')]
    out = Path('dist') / f'neoboard-{asset}.zip'
    out.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as bundle:
        for path in files:
            bundle.write(path, path.name)
        bundle.write('rune/LICENSE', 'LICENSE-Rune')
        bundle.write('vendor/winit-compat/winit-0.30.13/LICENSE', 'LICENSE-winit')
    outputs = [out]
    if system != 'windows':
        with tempfile.TemporaryDirectory() as tmp:
            stage = Path(tmp)
            app = stage / ('NeoBoard.app' if system == 'darwin' else 'neoboard')
            bin_dir = app / 'Contents/MacOS' if system == 'darwin' else app
            bin_dir.mkdir(parents=True)
            for path in files:
                shutil.copy2(path, bin_dir / path.name)
            shutil.copy2('rune/LICENSE', bin_dir / 'LICENSE-Rune')
            shutil.copy2('vendor/winit-compat/winit-0.30.13/LICENSE', bin_dir / 'LICENSE-winit')
            if system == 'darwin':
                (app/'Contents/Info.plist').write_bytes(plistlib.dumps(dict(CFBundleExecutable='neochat-desktop', CFBundleIdentifier='com.parkers0405.neoboard', CFBundleName='NeoBoard', CFBundlePackageType='APPL', CFBundleShortVersionString=version, CFBundleVersion=version, NSHighResolutionCapable=True)))
                subprocess.run(['codesign','--force','--deep','--sign','-',app],check=True)
                subprocess.run(['codesign','--verify','--deep','--strict',app],check=True)
                (stage/'Applications').symlink_to('/Applications')
                dmg = out.with_suffix('.dmg')
                subprocess.run(['hdiutil','create','-volname','NeoBoard','-srcfolder',stage,'-ov','-format','UDZO',dmg],check=True)
                subprocess.run(['hdiutil','verify',dmg],check=True)
                outputs.append(dmg)
            archive = out.with_suffix('.tar.gz')
            with tarfile.open(archive,'w:gz') as tar:
                tar.add(app,arcname=app.name)
            outputs.append(archive)
    for item in outputs:
        with item.open('rb') as f:
            digest = hashlib.file_digest(f, 'sha256').hexdigest()
        item.with_name(item.name+'.sha256').write_text(f'{digest}  {item.name}\n', encoding='ascii')


if __name__ == '__main__':
    package(sys.argv[1])