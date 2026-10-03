#!/usr/bin/env python3
"""Download reviewed inputs on the qualification host using its AWS instance role.

The manifest contains files with member, bytes and sha256. Objects live under
inputs/. Every downloaded byte is verified before publication; identical files
can be reused on resume. This tool does not create cloud resources.
"""
import argparse
import json
from pathlib import Path
import tempfile

from deploy_windows_payloads import sha, target_path


def transfer(client, bucket, root, entry):
    destination = target_path(root, entry['member'])
    def matches(path):
        return path.is_file() and path.stat().st_size == entry['bytes'] and sha(path) == entry['sha256']
    reused = destination.exists()
    if reused:
        if not matches(destination):
            raise ValueError(f'existing input differs: {entry["member"]}')
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
            temporary = Path(stream.name)
        try:
            client.download_file(bucket, 'inputs/' + entry['member'], str(temporary))
            if not matches(temporary):
                raise ValueError(f'download digest or length mismatch: {entry["member"]}')
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)
    return {'member': entry['member'], 'bytes': entry['bytes'], 'sha256': entry['sha256'],
            'reused': reused, 'downloaded_bytes_verified': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--bucket', required=True)
    parser.add_argument('--region', required=True)
    args = parser.parse_args()
    entries = json.loads(args.manifest.read_text())['files']
    root = args.root.resolve()
    names = [str(target_path(root, entry['member'])).casefold() for entry in entries]
    if len(set(names)) != len(names):
        raise ValueError('duplicate Windows input destinations')
    import boto3
    client = boto3.client('s3', region_name=args.region)
    records = []
    root.mkdir(parents=True, exist_ok=True)
    for entry in entries:
        records.append(transfer(client, args.bucket, root, entry))
        temporary = root / 'input-downloads.tmp'
        temporary.write_text(json.dumps({'manifest_sha256': sha(args.manifest),
                                         'files': records}, indent=2) + '\n')
        temporary.replace(root / 'input-downloads.json')
        print(json.dumps({'completed': len(records), 'total': len(entries),
                          'member': entry['member']}), flush=True)


if __name__ == '__main__':
    main()
