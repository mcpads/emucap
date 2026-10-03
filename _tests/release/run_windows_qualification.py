#!/usr/bin/env python3
"""Run the full qualification batch and checkpoint compact evidence to S3.

Start this wrapper in the host's durable executor. Only batch.json and the runner
receipt leave the host. Images, snapshots, dumps and emulator logs stay local.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time


def publish(client, bucket, prefix, path, previous):
    if not path.is_file():
        return previous
    if path.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('compact result exceeds 8 MiB; retain locally for inspection')
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != previous:
        client.put_object(Bucket=bucket, Key=prefix + '/' + path.name,
                          Body=data, ContentType='application/json',
                          Metadata={'sha256': digest})
    return digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--bucket', required=True)
    parser.add_argument('--region', required=True)
    parser.add_argument('--prefix', required=True)
    args = parser.parse_args()
    if not args.prefix.startswith('results/') or '..' in args.prefix or '\\' in args.prefix:
        raise ValueError('result prefix must stay under results/')
    import boto3
    from botocore.config import Config
    client = boto3.client('s3', region_name=args.region,
                          config=Config(connect_timeout=5, read_timeout=10,
                                        retries={'max_attempts': 2, 'mode': 'standard'}))
    args.output.mkdir(parents=True, exist_ok=True)
    # Exclusive ownership prevents two wrappers from operating the same profiles.
    lock = args.output / 'runner.lock'
    with lock.open('x') as stream:
        import os
        stream.write(str(os.getpid()))
    started = time.time()
    previous = None
    errors = []
    batch = args.output / 'batch.json'
    script = Path(__file__).resolve().parents[1] / 'live/qualification_batch.py'
    process = None
    try:
        with (args.output / 'runner.log').open('a', encoding='utf-8') as log:
            process = subprocess.Popen([sys.executable, str(script), '--manifest',
                                        str(args.manifest.resolve(strict=True)), '--output',
                                        str(args.output.resolve())], stdout=log, stderr=log)
            while True:
                try:
                    previous = publish(client, args.bucket, args.prefix, batch, previous)
                except Exception as error:
                    # Keep running guest checks if checkpoint storage is unavailable.
                    errors = (errors + [str(error)])[-10:]
                code = process.poll()
                if code is not None:
                    break
                time.sleep(15)
        # A final upload failure is visible as a failed wrapper, with local evidence intact.
        previous = publish(client, args.bucket, args.prefix, batch, previous)
        receipt = args.output / 'runner-result.json'
        receipt.write_text(json.dumps({'exit_code': code, 'started': started,
                                       'finished': time.time(), 'batch_sha256': previous,
                                       'checkpoint_errors': errors}, indent=2) + '\n')
        publish(client, args.bucket, args.prefix, receipt, None)
        raise SystemExit(code)
    finally:
        # If this wrapper is interrupted while its child survives, leave the
        # ownership marker for inspection instead of admitting a duplicate batch.
        if process is None or process.poll() is not None:
            lock.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
