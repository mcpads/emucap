"""Retain a native Windows build and its imported DLL closure for runtime checks."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from native_build import payload_hashes, sha


def imports(binary):
    output = subprocess.check_output(['objdump', '-p', str(binary)], text=True)
    return re.findall(r'DLL Name:\s*(\S+)', output)


def verify_np2kai_exports(binary):
    # Check the same entrypoints the Rust host will resolve, before publishing a
    # DLL that links successfully but cannot serve the adapter.
    import ctypes
    source = Path(__file__).resolve().parents[2] / 'src/np2kai_adapter/ffi.rs'
    names = re.findall(r'symbol!\(\s*"([^"]+)"', source.read_text())
    if not names:
        raise ValueError('NP2kai host entrypoints were not found')
    library = ctypes.CDLL(str(binary.resolve()))
    missing = [name for name in names if not hasattr(library, name)]
    if missing:
        raise ValueError(f'NP2kai DLL is missing host entrypoints: {missing}')


def find_file(directory, name):
    if not directory.is_dir():
        return None
    return next((p for p in directory.iterdir()
                 if p.name.lower() == name.lower() and p.is_file()), None)


def collect_imports(payload, search_dirs, system_dirs, inspect=imports):
    queue = [p for p in payload.rglob('*') if p.suffix.lower() in ('.exe', '.dll')]
    seen = set()
    while queue:
        binary = queue.pop()
        if binary in seen:
            continue
        seen.add(binary)
        for name in inspect(binary):
            if Path(name).name != name or '/' in name or '\\' in name:
                raise ValueError(f'invalid PE import: {name}')
            # API-set contracts are supplied by the Windows loader.
            if name.lower().startswith(('api-ms-win-', 'ext-ms-win-')):
                continue
            existing = find_file(binary.parent, name) or find_file(payload, name)
            if existing:
                queue.append(existing)
                continue
            source = next((p for directory in search_dirs
                           if (p := find_file(directory, name))), None)
            if source:
                destination = payload / source.name
                shutil.copy2(source, destination)
                queue.append(destination)
            elif not any(find_file(directory, name) for directory in system_dirs):
                raise ValueError(f'unresolved import {name} in {binary}')


def stage(binary, directory, tree=False, includes=()):
    if not binary.is_file():
        raise ValueError(f'missing native binary: {binary}')
    if directory.exists():
        raise ValueError('payload directory must be absent')
    directory.mkdir(parents=True)
    excluded = {'.pdb', '.lib', '.exp', '.ilk', '.obj', '.iobj', '.ipdb', '.log', '.a', '.o'}
    paths = binary.parent.rglob('*') if tree else [binary]
    for source in paths:
        if source.is_symlink():
            raise ValueError(f'symlink build output: {source}')
        if source.is_file() and source.suffix.lower() not in excluded:
            target = directory / source.relative_to(binary.parent)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)

    for source in includes:
        if source.is_symlink() or not source.exists():
            raise ValueError(f'missing or symlink runtime input: {source}')
        destination = directory / source.name
        if destination.exists():
            raise ValueError(f'conflicting runtime input: {source}')
        if source.is_dir():
            if any(p.is_symlink() for p in source.rglob('*')):
                raise ValueError(f'symlink runtime directory: {source}')
            shutil.copytree(source, destination)
        else:
            shutil.copy2(source, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adapter', required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--tree', action='store_true')
    parser.add_argument('--include', type=Path, action='append', default=[],
                        help='Required runtime file or directory retained by basename')
    parser.add_argument('--directory', type=Path, default=Path('native-payload'))
    parser.add_argument('--dll-directory', type=Path, action='append', default=[])
    args = parser.parse_args()
    stage(args.binary, args.directory, args.tree, args.include)
    system = Path(os.environ['SystemRoot']) / 'System32'
    collect_imports(args.directory, [args.binary.parent, *args.dll_directory], [system])
    if args.adapter == 'np2kai':
        verify_np2kai_exports(args.directory / args.binary.name)
    record = {
        'schema': 1, 'adapter': args.adapter,
        'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
        'executable': args.binary.name,
        'toolchain_sha256': sha(Path('native-toolchain.txt')),
        'files': payload_hashes(args.directory),
    }
    (args.directory / 'NATIVE-BUILD.json').write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__':
    main()
