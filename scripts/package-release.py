"""Package native builds; run on the target machine, from the repo root."""
import hashlib
import json
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

# A Developer ID identity signs for distribution; without one the macOS build is ad-hoc signed.
IDENTITY = os.environ.get('NEOBOARD_CODESIGN_IDENTITY') or '-'
NOTARY_PROFILE = os.environ.get('NEOBOARD_NOTARY_PROFILE')


def sign(path):
    # Notarization requires the hardened runtime and a secure timestamp on every executable.
    release = [] if IDENTITY == '-' else ['--options', 'runtime', '--timestamp']
    subprocess.run(['codesign', '--force', '--sign', IDENTITY, *release, path], check=True)


def notarize(path):
    if not NOTARY_PROFILE:
        return False
    keychain = os.environ.get('NEOBOARD_NOTARY_KEYCHAIN')
    auth = ['--keychain-profile', NOTARY_PROFILE, *(['--keychain', keychain] if keychain else [])]
    result = json.loads(subprocess.run(['xcrun', 'notarytool', 'submit', str(path), *auth, '--wait',
        '--output-format', 'json'], text=True, stdout=subprocess.PIPE, check=True).stdout)
    if result.get('status') != 'Accepted':
        log = subprocess.run(['xcrun', 'notarytool', 'log', result['id'], *auth], text=True, stdout=subprocess.PIPE).stdout
        sys.exit(f"Apple rejected {path.name} for notarization ({result.get('status')}):\n{log}")
    return True


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
    programs = files[:6]
    notices = [(path, path.name) for path in files[6:]] + [(Path('rune/LICENSE'), 'LICENSE-Rune'), (Path('vendor/winit-compat/winit-0.30.13/LICENSE'), 'LICENSE-winit')]
    if system == 'darwin':
        # Sign before archiving so the portable ZIP, the app and the tarball all carry the same signed programs.
        for path in programs:
            sign(path)
    out = Path('dist') / f'neoboard-{asset}.zip'
    out.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as bundle:
        for path in files:
            bundle.write(path, path.name)
        bundle.write('rune/LICENSE', 'LICENSE-Rune')
        bundle.write('vendor/winit-compat/winit-0.30.13/LICENSE', 'LICENSE-winit')
    outputs = [out]
    if system == 'darwin':
        # A ZIP cannot hold a stapled ticket; Gatekeeper looks its programs up online.
        notarize(out)
    if system != 'windows':
        with tempfile.TemporaryDirectory() as tmp:
            stage = Path(tmp)
            app = stage / ('NeoBoard.app' if system == 'darwin' else 'neoboard')
            bin_dir = app / 'Contents/MacOS' if system == 'darwin' else app
            bin_dir.mkdir(parents=True)
            if system == 'darwin':
                resources = app/'Contents/Resources'
                resources.mkdir()
                for path in programs:
                    shutil.copy2(path, bin_dir / path.name)
                # Only programs belong beside the executable: a signature on any other file there
                # lives in extended attributes, which archives and downloads drop.
                for path, name in notices:
                    shutil.copy2(path, resources / name)
                shutil.copy2('design/NeoBoard.icns', resources/'NeoBoard.icns')
                (app/'Contents/Info.plist').write_bytes(plistlib.dumps(dict(CFBundleExecutable='neochat-desktop', CFBundleIdentifier='com.parkers0405.neoboard', CFBundleName='NeoBoard', CFBundleIconFile='NeoBoard.icns', CFBundlePackageType='APPL', CFBundleShortVersionString=version, CFBundleVersion=version, NSHighResolutionCapable=True)))
                sign(app)
                subprocess.run(['codesign','--verify','--deep','--strict',app],check=True)
                if IDENTITY != '-':
                    with tempfile.TemporaryDirectory() as upload:
                        archive = Path(upload)/'NeoBoard.zip'
                        subprocess.run(['ditto','-c','-k','--keepParent',app,archive],check=True)
                        if notarize(archive):
                            subprocess.run(['xcrun','stapler','staple',app],check=True)
                            subprocess.run(['spctl','--assess','--type','execute','-vv',app],check=True)
            else:
                for path, name in notices:
                    shutil.copy2(path, bin_dir / name)
                for path in programs:
                    shutil.copy2(path, bin_dir / path.name)
            if system == 'darwin':
                (stage/'Applications').symlink_to('/Applications')
                dmg = out.with_suffix('.dmg')
                subprocess.run(['hdiutil','create','-volname','NeoBoard','-srcfolder',stage,'-ov','-format','UDZO',dmg],check=True)
                subprocess.run(['hdiutil','verify',dmg],check=True)
                if IDENTITY != '-':
                    sign(dmg)
                    if notarize(dmg):
                        subprocess.run(['xcrun','stapler','staple',dmg],check=True)
                outputs.append(dmg)
            archive = out.with_suffix('.tar.gz')
            with tarfile.open(archive,'w:gz') as tar:
                tar.add(app,arcname=app.name)
                if system == 'linux':
                    # Keep the updater-managed binary directory free of extra files.
                    share = 'neoboard-desktop-integration'
                    tar.add('desktop/ai.neoism.neochat.desktop', arcname=f'{share}/applications/ai.neoism.neochat.desktop')
                    tar.add('design/NeoBoard.png', arcname=f'{share}/icons/hicolor/256x256/apps/ai.neoism.neochat.png')
                    tar.add('desktop/LINUX-LAUNCHER.md', arcname=f'{share}/README.md')
            outputs.append(archive)
    for item in outputs:
        with item.open('rb') as f:
            digest = hashlib.file_digest(f, 'sha256').hexdigest()
        item.with_name(item.name+'.sha256').write_text(f'{digest}  {item.name}\n', encoding='ascii')


if __name__ == '__main__':
    package(sys.argv[1])