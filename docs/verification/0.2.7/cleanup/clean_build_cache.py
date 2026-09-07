"""Conservative Cargo cache cleanup. Explicit roots only; binaries are preserved.
No symlink traversal, no packaged data/session/config directories, no root removal.
Use --apply only after reviewing the dry-run report. No credential handling.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import struct

parser = argparse.ArgumentParser()
parser.add_argument('--apply', action='store_true')
parser.add_argument('--report', required=True)
parser.add_argument('roots', nargs='+')
args = parser.parse_args()

def protected(name):
    return name in {'data', 'sessions', '.git', 'session.json', 'history.jsonl'} or name.startswith('.minicore')

def runnable(path, info):
    if info.st_mode & 0o111:
        return True
    with path.open('rb') as stream:
        head = stream.read(64)
    if head.startswith((b'#!', b'MZ')):
        return True
    if head.startswith(b'\x7fELF') and len(head) >= 18:
        endian = 'little' if head[5] == 1 else 'big'
        return int.from_bytes(head[16:18], endian) in {2, 3}
    if len(head) >= 16:
        magic = head[:4]
        if magic in {b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}:
            return True
        if magic in {b'\xce\xfa\xed\xfe', b'\xcf\xfa\xed\xfe'}:
            return struct.unpack('<I', head[12:16])[0] in {2, 6, 8}
        if magic in {b'\xfe\xed\xfa\xce', b'\xfe\xed\xfa\xcf'}:
            return struct.unpack('>I', head[12:16])[0] in {2, 6, 8}
    return False

def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()

def identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_mode)

report = {'applied':args.apply, 'roots':[], 'preserved_binaries':[]}
candidates = []
seen = set()
for raw in args.roots:
    requested = Path(raw)
    assert requested.is_absolute() and not requested.is_symlink(), raw
    root = requested.resolve(strict=True)
    assert root != Path('/') and not any(protected(p) for p in root.parts), root
    profiles = []
    for parent in [root] + [p for p in root.iterdir() if p.is_dir() and not p.is_symlink() and not protected(p.name)]:
        for p in parent.iterdir():
            if p.is_dir() and not p.is_symlink() and not protected(p.name) and ((p / '.fingerprint').is_dir() or (p / 'incremental').is_dir()):
                profiles.append(p)
    profiles = list(dict.fromkeys(profiles))
    assert profiles, f'No recognized Cargo profiles in {root}; refusing broad cleanup'
    paths = []
    for profile in profiles:
        for name in ['incremental', '.fingerprint', 'build', 'deps']:
            base = profile / name
            if not base.is_dir() or base.is_symlink():
                continue
            for current, dirs, files in os.walk(base, followlinks=False):
                dirs[:] = [d for d in dirs if not protected(d) and not (Path(current) / d).is_symlink()]
                paths.extend(Path(current) / f for f in files if not protected(f))
        for p in profile.iterdir():
            if p.is_file() and not p.is_symlink() and not protected(p.name):
                info = p.stat()
                if info.st_mode & 0o111 or p.suffix in {'.exe', '.dylib', '.dll', '.so', '.rlib', '.rmeta', '.d', '.a', '.lib'} or p.name == '.cargo-lock':
                    paths.append(p)
    summary = {'root':str(root), 'candidate_files':0, 'candidate_bytes':0, 'preserved_binaries':0}
    for path in paths:
        if path in seen:
            continue
        seen.add(path)
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode):
            continue
        assert path.is_relative_to(root)
        if runnable(path, info):
            report['preserved_binaries'].append({'path':str(path), 'sha256':digest(path), 'bytes':info.st_size})
            summary['preserved_binaries'] += 1
        else:
            candidates.append((path, identity(info)))
            summary['candidate_files'] += 1
            summary['candidate_bytes'] += info.st_size
    report['roots'].append(summary)

if args.apply:
    # Refuse changed candidates before deleting anything; never follow symlinks.
    for path, expected in candidates:
        assert identity(path.lstat()) == expected, f'File changed during inventory: {path}'
    for path, expected in candidates:
        assert identity(path.lstat()) == expected, f'File changed during cleanup: {path}'
        path.unlink()
    for binary in report['preserved_binaries']:
        assert digest(Path(binary['path'])) == binary['sha256'], f"Preserved binary changed: {binary['path']}"
    report['binary_hash_check'] = 'PASS'
    report['deleted_files'] = len(candidates)
else:
    report['deleted_files'] = 0
Path(args.report).write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({k:v for k,v in report.items() if k != 'preserved_binaries'}, indent=2))
