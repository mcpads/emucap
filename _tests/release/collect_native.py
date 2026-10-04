"""Assemble all platform packages and reject stale or conflicting release inputs."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

from package_native import run, sha

ADAPTERS = ('mesen2', 'dolphin', 'ppsspp', 'mednafen', 'flycast', 'np2kai',
            'mupen64plus', 'openmsx', 'mame-pc98', 'mame-neogeo', 'pcsx2', 'desmume-nds', 'xemu')
PLATFORMS = ('linux', 'macos', 'windows')


def validate_selection(selection):
    expected = {(a, p) for a in ADAPTERS for p in PLATFORMS}
    actual = [(r['adapter'], r['platform']) for r in selection]
    if set(actual) != expected or len(actual) != len(expected):
        raise ValueError('release requires one package for every adapter and platform')
    for record in selection:
        if not str(record['run']).isdecimal(): raise ValueError('invalid run ID')


def validate_package(directory, source, adapter, platform):
    records = [json.loads(p.read_text()) for p in directory.glob('emucap-*.json')]
    if len(records) != 1: raise ValueError('expected one native package record')
    record = records[0]
    target = {'linux': 'x86_64-unknown-linux-gnu', 'macos': 'aarch64-apple-darwin',
              'windows': 'windows-x86_64'}[platform]
    if platform == 'macos' and adapter == 'pcsx2': target = 'x86_64-apple-darwin'
    version = tomllib.loads((source / 'Cargo.toml').read_text())['package']['version']
    if (record['adapter'], record['target'], record['version']) != (adapter, target, version):
        raise ValueError('native package identity mismatch')
    for name, digest in record['artifacts'].items():
        if Path(name).name != name or sha(directory / name) != digest:
            raise ValueError(f'native artifact digest mismatch: {name}')
    runtime_name = f'emucap-{version}-{adapter}-{target}'
    runtime = directory / (runtime_name + ('.zip' if platform == 'windows' else '.tar.gz'))
    if runtime.name not in record['artifacts']: raise ValueError('missing runtime archive')
    if platform == 'windows':
        with zipfile.ZipFile(runtime) as archive:
            manifest = json.loads(archive.read(runtime_name + '/NATIVE-PACKAGE.json'))
    else:
        with tarfile.open(runtime) as archive:
            manifest = json.load(archive.extractfile(runtime_name + '/NATIVE-PACKAGE.json'))
    for key in ('adapter', 'target', 'version', 'source_revision', 'executable'):
        if manifest[key] != record[key]: raise ValueError('runtime manifest identity mismatch')
    if manifest['executable'] not in manifest['files']:
        raise ValueError('runtime executable absent from file manifest')
    sources = [name for name in record['artifacts'] if name.endswith('-source.tar.gz')]
    if len(sources) != 1: raise ValueError('missing corresponding native source archive')
    owner = 'mame-pc98' if adapter == 'mame-neogeo' else adapter
    tracked = run('git', '-C', source, 'ls-files', f'adapters/{owner}', f'adapters/{adapter}').splitlines()
    required = [p for p in tracked if Path(p).suffix not in ('.md', '.sh', '.ps1')]
    with tarfile.open(directory / sources[0]) as archive:
        for rel in required:
            path = sources[0][:-7] + '/recipes/' + rel
            stream = archive.extractfile(path)
            if stream is None or stream.read() != (source / rel).read_bytes():
                raise ValueError(f'package incompatible with release source: {rel}')
    return record


def merge_file(path, output):
    target = output / path.name
    if target.exists():
        if sha(target) != sha(path): raise ValueError(f'conflicting release artifact: {path.name}')
    else:
        shutil.move(path, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selection', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repo', required=True)
    args = parser.parse_args()
    selection = json.loads(args.selection.read_text())
    validate_selection(selection)
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()): raise ValueError('release output must be empty')
    records = []
    for item in selection:
        adapter, platform, producer = item['adapter'], item['platform'], str(item['run'])
        info = json.loads(run('gh', 'api', f'repos/{args.repo}/actions/runs/{producer}'))
        # A matrix can contain unrelated failures; only completed package artifacts
        # from successful packaging steps are selected and independently verified.
        if info['status'] != 'completed' or info['event'] == 'pull_request':
            raise ValueError('native producer must be a completed trusted run')
        artifact = f'native-package-{adapter}-{platform}'
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            subprocess.run(['gh', 'run', 'download', producer, '--repo', args.repo,
                            '--name', artifact, '--dir', temporary], check=True)
            record = validate_package(directory, args.source, adapter, platform)
            records.append(dict(record, platform=platform, workflow_run=int(producer)))
            for name in [*record['artifacts'], f"emucap-{record['version']}-{adapter}-{record['target']}.json"]:
                merge_file(directory / name, args.output)
        print(f'verified {adapter}/{platform}', flush=True)
    index = dict(core_revision=run('git', '-C', args.source, 'rev-parse', 'HEAD'), packages=records)
    (args.output / 'NATIVE-PACKAGES.json').write_text(json.dumps(index, indent=2) + '\n')
    checksums = ''.join(f'{sha(p)}  {p.name}\n' for p in sorted(args.output.iterdir()))
    (args.output / 'NATIVE-SHA256SUMS').write_text(checksums)


if __name__ == '__main__': main()
