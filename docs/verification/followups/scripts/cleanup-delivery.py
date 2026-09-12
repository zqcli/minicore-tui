"""Remove only regenerable incremental directories from explicit owned roots."""
import hashlib
import json
import os
from pathlib import Path
import shutil

base = Path('/root/minicore-tui-027-RXdxEP')
roots = [base / name for name in ('target-linux', 'target-msrv', 'target-macos-llvm')]
roots = [root for root in roots if root.is_dir() and not root.is_symlink()]
for proc in Path('/proc').iterdir():
    if not proc.name.isdigit():
        continue
    try:
        name = (proc / 'comm').read_text().strip()
        if name not in {'cargo', 'rustc', 'rustfmt', 'clippy-driver', 'clang', 'ld.lld', 'dsymutil'}:
            continue
        cwd = (proc / 'cwd').resolve(strict=True)
        owned = Path('/root/minicore-followups.6Dpa6e')
        assert not (cwd == owned or owned in cwd.parents), f'active owned build blocks cleanup: {proc.name}'
        assert not any(cwd == root or root in cwd.parents or cwd == root.parent for root in roots), f'active build blocks cleanup: {proc.name}'
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        continue

def retained_file(path):
    return (path.stat().st_mode & 0o111 != 0 or path.suffix in {'.rlib', '.so', '.dylib', '.a', '.dll', '.exe'} or any(part.endswith('.dSYM') for part in path.parts))

def checksum(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for data in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(data)
    return digest.hexdigest()

retained = {}
removable = []
for root in roots:
    for directory, dirs, files in os.walk(root, followlinks=False):
        parent = Path(directory)
        dirs[:] = [name for name in dirs if not (parent / name).is_symlink()]
        if parent.name == 'incremental':
            assert not parent.is_symlink()
            candidates = [path for path in parent.rglob('*') if path.is_file() and not path.is_symlink()]
            if not any(retained_file(path) for path in candidates):
                removable.append((parent, sum(path.stat().st_size for path in candidates)))
                dirs.clear()
                continue
        for name in files:
            path = parent / name
            if not path.is_symlink() and retained_file(path):
                retained[str(path)] = checksum(path)
report = {'roots': [str(root) for root in roots], 'retained_count': len(retained),
          'removed': [{'path': str(path), 'logical_bytes': size} for path, size in removable],
          'logical_bytes_removed': sum(size for _, size in removable)}
Path('/root/minicore-followups.6Dpa6e/accepted/cleanup-preserved-hashes.json').write_text(json.dumps(retained, indent=2) + '\n')
for path, _ in removable:
    shutil.rmtree(path)
for name, digest in retained.items():
    assert checksum(Path(name)) == digest, f'preserved artifact changed: {name}'
report['retained_hashes_verified'] = True
print(json.dumps(report, indent=2))
