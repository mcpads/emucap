#!/usr/bin/env python3
"""Verify a deployed Windows qualification layout and freeze its batch inputs.

Run on the validation host after deploying CI payloads and private inputs. This
checks deployment bytes and profile paths; it does not establish game execution.
"""
import argparse
import json
from pathlib import Path
import tomllib

from qualification_batch import prepare, sha


def relative_path(root, member):
    path = root / member
    resolved = path.resolve(strict=True)
    if not resolved.is_relative_to(root.resolve()):
        raise ValueError(f'input escapes deployment: {member}')
    current = path
    while current != root:
        if current.is_symlink():
            raise ValueError(f'symlink deployment input: {current}')
        current = current.parent
    return path


def check_bytes(root, member, expected):
    path = relative_path(root, member)
    if not path.is_file() or path.stat().st_size != expected['bytes'] or sha(path) != expected['sha256']:
        raise ValueError(f'deployed bytes differ: {member}')
    return path


def freeze(root, layout, media, firmware, draft):
    root = root.resolve(strict=True)
    files = set()
    ready = {row['id']: row for row in media['rows'] if row['status'] == 'media_ready'}
    if {row['id'] for row in draft['rows']} != set(ready):
        raise ValueError('batch rows differ from prepared media inventory')
    for row in draft['rows']:
        item = ready[row['id']]
        for entry in item['files']:
            files.add(check_bytes(root, f"media/{item['id']}/{entry['member']}", entry))
        profile_path = relative_path(root, row['profile'])
        profile = json.loads(profile_path.read_text(encoding='utf-8'))
        expected_content = root / 'media' / item['id'] / item['content']
        if Path(profile['launch_plan']['content_path']).resolve(strict=True) != expected_content.resolve():
            raise ValueError(f'profile content differs from inventory: {row["id"]}')
        for key, member in layout['adapter_env'][item['adapter']].items():
            target = relative_path(root, member)
            actual = profile['env'].get(key)
            if actual is None or Path(actual).resolve(strict=True) != target.resolve():
                raise ValueError(f'profile deployment differs: {row["id"]}/{key}')
    for entry in firmware['files']:
        files.add(check_bytes(root, f"firmware/{entry['group']}/{entry['member']}", entry))
    core = relative_path(root, layout['core_directory'])
    cargo = relative_path(root, 'repo/Cargo.toml')
    with cargo.open('rb') as stream:
        binaries = tomllib.load(stream)['bin']
    for binary in binaries:
        if not (core / (binary['name'] + '.exe')).is_file():
            raise ValueError(f'missing Windows host: {binary["name"]}')
    files.add(cargo)
    directories = [core]
    for adapter in layout['adapter_env']:
        directories.append(relative_path(root, layout['native_directory'] + '/' + adapter))
    for directory in directories:
        if not directory.is_dir():
            raise ValueError(f'missing runtime directory: {directory}')
        members = list(directory.rglob('*'))
        if not any(path.is_file() for path in members):
            raise ValueError(f'empty runtime directory: {directory}')
        for path in members:
            relative_path(root, path.relative_to(root))
            if path.is_file():
                files.add(path)
    return {'rows': draft['rows'], 'artifacts': [
        {'path': path.relative_to(root).as_posix(), 'sha256': sha(path)}
        for path in sorted(files)],
        'unprepared_media': [row['id'] for row in media['rows'] if row['status'] != 'media_ready']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    for name in ('layout', 'media', 'firmware', 'draft'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    records = [json.loads(getattr(args, name).read_text(encoding='utf-8'))
               for name in ('layout', 'media', 'firmware', 'draft')]
    manifest = freeze(args.root, *records)
    destination = args.root / 'runtime-batch.json'
    # Never replace the identities used by an existing batch.
    with destination.open('x', encoding='utf-8') as stream:
        json.dump(manifest, stream, indent=2)
        stream.write('\n')
    prepare(destination.resolve(strict=True))
    print(json.dumps({'manifest': str(destination), 'rows': len(manifest['rows']),
                      'files': len(manifest['artifacts']), 'unprepared_media': manifest['unprepared_media']}))


if __name__ == '__main__':
    main()
