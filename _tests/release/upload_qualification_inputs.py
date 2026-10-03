#!/usr/bin/env python3
"""Upload reviewed private qualification inputs to an operator-owned S3 bucket.

Manifest files have source, member, bytes and sha256. Objects stay under inputs/;
only object metadata is read back. Remote deployment must verify downloaded bytes.
Requires the operator's authenticated AWS CLI. No cloud resources are created.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess


def aws(region, *args, missing=False):
    command = ['aws', '--region', region, *args, '--output', 'json']
    result = subprocess.run(command, capture_output=True, text=True)
    if missing and result.returncode and ('404' in result.stderr or 'NoSuchKey' in result.stderr):
        return None
    if result.returncode:
        raise RuntimeError(result.stderr.strip())
    return json.loads(result.stdout) if result.stdout.strip() else {}


def transfer(bucket, region, entry):
    source = Path(entry['source']).resolve(strict=True)
    member = PurePosixPath(entry['member'])
    if member.is_absolute() or '..' in member.parts or '\\' in entry['member']:
        raise ValueError('invalid input member')
    with source.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    if source.stat().st_size != entry['bytes'] or actual != entry['sha256']:
        raise ValueError(f'changed private input: {entry["member"]}')
    key = 'inputs/' + member.as_posix()
    def head():
        return aws(region, 's3api', 'head-object', '--bucket', bucket, '--key', key, missing=True)
    def matches(value):
        return value is not None and value['ContentLength'] == entry['bytes'] and value.get('Metadata', {}).get('sha256') == actual
    before = head()
    if before is not None and not matches(before):
        raise ValueError(f'existing object differs: {key}; choose a new input key')
    if before is None:
        subprocess.run(['aws', '--region', region, 's3', 'cp', str(source), f's3://{bucket}/{key}',
                        '--only-show-errors', '--metadata', 'sha256=' + actual], check=True)
    after = head()
    if not matches(after):
        raise ValueError(f'uploaded object metadata differs: {key}')
    return {'key': key, 'bytes': entry['bytes'], 'sha256': actual, 'etag': after['ETag'],
            'uploaded': before is None, 'downloaded_bytes_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--bucket', required=True)
    parser.add_argument('--region', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    files = json.loads(args.manifest.read_text())['files']
    if len({x['member'] for x in files}) != len(files):
        raise ValueError('duplicate input members')
    records = []
    with ThreadPoolExecutor(max_workers=2) as pool:
        pending = {pool.submit(transfer, args.bucket, args.region, entry): entry for entry in files}
        for future in as_completed(pending):
            entry = pending[future]
            try:
                result = future.result()
            except Exception as error:
                result = {'key': 'inputs/' + entry['member'], 'error': str(error)}
            records.append(result)
            temporary = args.output.with_suffix('.tmp')
            temporary.write_text(json.dumps({'bucket': args.bucket, 'objects': records}, indent=2) + '\n')
            temporary.replace(args.output)
            print(json.dumps({'member': entry['member'], 'completed': len(records),
                              'total': len(files), 'ok': 'error' not in result}), flush=True)
    raise SystemExit(1 if any('error' in r for r in records) else 0)


if __name__ == '__main__':
    main()
