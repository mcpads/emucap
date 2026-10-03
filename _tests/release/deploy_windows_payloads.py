#!/usr/bin/env python3
"""Deploy already-downloaded, digest-pinned qualification ZIPs on the Windows host.

Plan archives contain path, sha256, destination, and optional inner_zip/prefix.
Paths are relative to the plan; destinations are relative to --root. Existing
identical files permit resume. Different files are never overwritten.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tempfile
import zipfile


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def relative(name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or '..' in path.parts or ':' in name or '\\' in name:
        raise ValueError(f'unsafe payload path: {name}')
    return path


def target_path(root, name):
    path = root.joinpath(*relative(name).parts)
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError(f'payload destination escapes root: {name}')
    current = path
    while current != root:
        if current.is_symlink():
            raise ValueError(f'symlink payload destination: {name}')
        current = current.parent
    return path


def deploy(root, plan_path):
    plan = json.loads(plan_path.read_text())
    root = root.resolve()
    records = []
    # Reject a damaged download before changing any deployed file.
    for spec in plan['archives']:
        path = plan_path.parent / spec['path']
        if sha(path) != spec['sha256']:
            raise ValueError(f'archive digest mismatch: {spec["path"]}')
        relative(spec['destination'])
    for spec in plan['archives']:
        path = plan_path.parent / spec['path']
        with ExitStack() as stack:
            archive = stack.enter_context(zipfile.ZipFile(path))
            if spec.get('inner_zip'):
                inner = stack.enter_context(tempfile.TemporaryFile())
                with archive.open(spec['inner_zip']) as stream:
                    shutil.copyfileobj(stream, inner)
                inner.seek(0)
                archive = stack.enter_context(zipfile.ZipFile(inner))
            prefix = spec.get('prefix', '')
            if prefix:
                relative(prefix)
                prefix = prefix.rstrip('/') + '/'
            selected = []
            names = set()
            for entry in archive.infolist():
                relative(entry.filename)
                if entry.is_dir() or not entry.filename.startswith(prefix):
                    continue
                if (entry.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError(f'symlink ZIP member: {entry.filename}')
                member = entry.filename[len(prefix):]
                destination = target_path(root, spec['destination'] + '/' + member)
                canonical = str(destination).casefold()
                if canonical in names:
                    raise ValueError(f'duplicate Windows destination: {member}')
                names.add(canonical)
                selected.append((entry, destination))
            if not selected:
                raise ValueError(f'empty payload selection: {spec["path"]}')
            for entry, destination in selected:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
                    temporary = Path(output.name)
                    try:
                        with archive.open(entry) as stream:
                            shutil.copyfileobj(stream, output)
                    except BaseException:
                        output.close()
                        temporary.unlink()
                        raise
                try:
                    if destination.exists():
                        if not destination.is_file() or sha(destination) != sha(temporary):
                            raise ValueError(f'existing payload differs: {destination}')
                    else:
                        temporary.replace(destination)
                finally:
                    temporary.unlink(missing_ok=True)
            records.append({'archive': spec['path'], 'sha256': spec['sha256'],
                            'destination': spec['destination'], 'files': len(selected)})
    record = {'plan_sha256': sha(plan_path), 'archives': records, 'runtime_validated': False}
    (root / 'payload-deployment.json').write_text(json.dumps(record, indent=2) + '\n')
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--plan', type=Path, required=True)
    args = parser.parse_args()
    record = deploy(args.root, args.plan.resolve(strict=True))
    print(json.dumps({'archives': len(record['archives']),
                      'files': sum(x['files'] for x in record['archives'])}))


if __name__ == '__main__':
    main()
