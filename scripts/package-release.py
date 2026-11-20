"""Package native builds; run on the target machine, from the repo root."""
import hashlib
import os
from pathlib import Path
import platform
import sys
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
    with out.open('rb') as f:
        digest = hashlib.file_digest(f, 'sha256').hexdigest()
    out.with_suffix('.zip.sha256').write_text(f'{digest}  {out.name}\n', encoding='ascii')


if __name__ == '__main__':
    package(sys.argv[1])