"""Input identity and verified runtime cache for native build CI (no publication)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
OUTPUTS = {
    'mesen2': ('work/mesen/bin/win-x64/Release', 'Mesen.exe'),
    'dolphin': ('work/dolphin-src/Binary/x64', 'Dolphin.exe'),
    'ppsspp': ('work/ppsspp/build-headless/Release', 'PPSSPPHeadless.exe'),
}


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def identity(root, adapter, toolchain):
    if adapter not in OUTPUTS:
        raise ValueError(f'unknown native recipe: {adapter}')
    # These PowerShell recipes consume only their adapter trees. The MSYS2
    # recipes own _common; changing their helpers must not rebuild MSVC outputs.
    paths = subprocess.check_output(
        ['git', 'ls-files', '-z', '--', f'adapters/{adapter}',
         '_tests/release/native_build.py',
         'tools/deploy/public/.github/workflows/native-adapters.yml',
         '.github/workflows/native-adapters.yml'], cwd=root,
    ).decode().split('\0')
    files = {}
    for name in sorted(set(filter(None, paths))):
        if name.endswith('.md'):
            continue
        path = root / name
        if path.is_symlink():
            raise ValueError(f'symlink build input: {name}')
        files[name] = sha(path)
    if f'adapters/{adapter}/upstream.lock' not in files:
        raise ValueError('native identity requires a tracked upstream lock')
    # The public workflow replaces the private overlay at sync time.
    workflow = 'tools/deploy/public/.github/workflows/native-adapters.yml'
    if workflow in files:
        files['.github/workflows/native-adapters.yml'] = files.pop(workflow)
    record = dict(schema=1, adapter=adapter, target='windows-x86_64',
                  toolchain=toolchain, inputs=files)
    encoded = json.dumps(record, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(encoded).hexdigest(), record


def payload_hashes(directory):
    files = {}
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise ValueError(f'symlink payload: {path}')
        if path.is_file() and path != directory / 'NATIVE-BUILD.json':
            files[path.relative_to(directory).as_posix()] = sha(path)
    return files


def verify(directory, key):
    record = json.loads((directory / 'NATIVE-BUILD.json').read_text())
    if record['identity'] != key or record['files'] != payload_hashes(directory):
        raise ValueError('native cache identity or payload mismatch')
    if not record['files']:
        raise ValueError('empty native payload')
    return record


def stage(adapter, directory, key, inputs):
    if directory.exists():
        raise ValueError('staging directory must be absent')
    relative, binary = OUTPUTS[adapter]
    source = ROOT / 'adapters' / adapter / relative
    if not (source / binary).is_file():
        raise ValueError(f'missing native executable: {binary}')
    excluded = {'.pdb', '.lib', '.exp', '.ilk', '.obj', '.iobj', '.ipdb', '.log'}
    for path in source.rglob('*'):
        if '.git' in path.relative_to(source).parts:
            continue
        if path.is_symlink():
            raise ValueError(f'symlink build output: {path}')
        if path.is_file() and path.suffix.lower() not in excluded:
            target = directory / path.relative_to(source)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
    record = dict(identity=key, build_inputs=inputs, files=payload_hashes(directory))
    (directory / 'NATIVE-BUILD.json').write_text(json.dumps(record, indent=2) + '\n')
    verify(directory, key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('operation', choices=['identity', 'stage', 'verify'])
    parser.add_argument('--adapter', choices=OUTPUTS, required=True)
    parser.add_argument('--toolchain', required=True)
    parser.add_argument('--directory', type=Path, default=Path('native-payload'))
    args = parser.parse_args()
    key, inputs = identity(ROOT, args.adapter, args.toolchain)
    if args.operation == 'identity':
        print(key)
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
                stream.write(f'key={key}\n')
    elif args.operation == 'stage':
        stage(args.adapter, args.directory, key, inputs)
    else:
        verify(args.directory, key)


if __name__ == '__main__':
    main()
