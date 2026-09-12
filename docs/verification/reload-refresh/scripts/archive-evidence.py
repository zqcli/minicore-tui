#!/usr/bin/env python3
"""Archive this owned corrective run; retain original terminal and log bytes."""
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
import shutil

root = Path(__file__).resolve().parents[1]
dest = Path('/Users/zzq/Develops/minicore-tui/docs/verification/reload-refresh')
assert not dest.exists()

def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()

hashes = {}
for line in (root / 'hashes.txt').read_text().splitlines():
    digest, name = line.split(maxsplit=1)
    assert sha(root / name) == digest
    hashes[name] = digest

native = {}
prefixes = {'followups': 'minicore-native-followups-',
            'session': 'minicore-session-native-',
            'stream_regression': 'minicore-stream-smooth-iterm-'}
for kind, prefix in prefixes.items():
    for profile in ('debug', 'release'):
        result = json.loads((root / f'logs/native-{kind}-{profile}.log').read_text())
        assert result['status'] == 'PASS'
        assert result['tui_sha256'] == hashes[f'artifacts/minicore-tui-{profile}']
        assert result['agent_sha256'] == hashes[f'artifacts/minicore-agent-{profile}']
        if kind == 'followups':
            assert result['request_count'] == 19
        source = Path(result['evidence_dir']).resolve()
        assert source.name == 'evidence' and source.parent.name.startswith(prefix)
        assert not any(path.is_symlink() for path in source.rglob('*'))
        name = f'{kind.replace("_regression", "")}-{profile}'
        target = root / 'native' / name
        shutil.copytree(source, target)
        assert (target / 'result.json').is_file()
        native[name] = f'native/{name}/result.json'

tty = root / 'native/tty'
tty.mkdir()
for mode in ('normal', 'panic'):
    shutil.copy2(root / f'native-pty-final-{mode}.log', tty / f'{mode}.log')
shutil.copy2(root / 'native-pty-final-result.json', tty / 'result.json')
assert json.loads((tty / 'result.json').read_text())['status'] == 'PASS'

manifest = {
    'schema_version': 1,
    'recorded_at_utc': datetime.now(timezone.utc).isoformat(),
    'scope': 'Corrective public /reload exact-turn reread and wait identity fencing',
    'tui': {'commit': 'a604e55baf74422722545b86bb9c6d30c31473e6', 'version': '0.2.8',
            'archive_sha256': hashes['source/minicore-tui.tar'],
            'stable': {'passed': 508, 'ignored': 19},
            'msrv': {'passed': 508, 'ignored': 19}},
    'agent': {'commit': 'f1697f78ce48c8f5f3fde0dc9903c153022bfd9e', 'version': '0.3.3',
              'archive_sha256': hashes['source/minicore-agent.tar'],
              'macos_artifacts_reused_unchanged': True,
              'suite_evidence': 'Prior exact-source followups acceptance: 369 passed / 2 ignored each',
              'linux_build': 'Fresh build from the fixed archive for paired E2E'},
    'runtime': {'version': '0.4.1', 'revision': '6cd2bdbc634437dea925495c61c7eb0be10ba171'},
    'e2e': {'stable_passed': 18, 'msrv_passed': 18},
    'native_results': native,
    'tty_result': 'native/tty/result.json',
    'tui_debug_dsym_uuid': '4C4C4459-5555-3144-A1E3-4D9E5D5CD3E3',
    'bundle_root': str(root),
    'remote_root': '/root/minicore-followups.6Dpa6e/reload-refresh',
    'bundle_files_sha256': hashes,
    'binary_and_source_archives_in_git': False,
    'installation': 'installation.log',
    'backup': '/Users/zzq/Develops/minicore-tui/target/preserved-before-reload-refresh-AdCugX',
    'no_user_process_restart': True,
    'no_user_config_or_store_edits': True,
    'new_hosted_ci_or_windows_acceptance': False,
}
(root / 'provenance.json').write_text(json.dumps(manifest, indent=2) + '\n')
dest.mkdir(parents=True)
for name in ('native', 'logs', 'scripts'):
    shutil.copytree(root / name, dest / name)
for name in ('provenance.json', 'hashes.txt', 'installation.log'):
    shutil.copy2(root / name, dest / name)
print(json.dumps({'archive': str(dest), 'native_results': native}, indent=2))
