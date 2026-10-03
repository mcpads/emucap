#!/usr/bin/env python3
"""Deliver CI archives directly to a prepared Windows SSM host, one fresh URL at a time.

Run locally with gh and AWS CLI credentials. The GitHub token remains local.
Only the expiring artifact URL reaches the host; archive bytes never pass locally.
The output ledger retains command IDs so an interrupted delivery can be resumed.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time

from inspect_github_artifact import artifact_download_url


def aws(region, *arguments, invocation=False):
    result = subprocess.run(['aws', '--region', region, *arguments, '--output', 'json'],
                            capture_output=True, text=True)
    if invocation and result.returncode and 'InvocationDoesNotExist' in result.stderr:
        return None
    if result.returncode:
        raise RuntimeError(f'AWS {arguments[0]} {arguments[1]} observation/request failed; retained commands require inspection')
    return json.loads(result.stdout)


def quote(value):
    return "'" + value.replace("'", "''") + "'"


def download_command(root, entry, url):
    if not re.fullmatch(r'downloads/[0-9]+\.zip', entry['path']):
        raise ValueError('unexpected archive destination')
    if not re.fullmatch(r'[0-9a-f]{64}', entry['sha256']):
        raise ValueError('invalid archive digest')
    script = f"""
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12
$root=[IO.Path]::GetFullPath({quote(root)}).TrimEnd('\\')
$path=Join-Path $root {quote(entry['path'])}
$expected={quote(entry['sha256'])}
try {{
  New-Item -ItemType Directory -Force (Split-Path $path) | Out-Null
  $reused=Test-Path -LiteralPath $path
  if (-not $reused) {{
    Invoke-WebRequest -Uri {quote(url)} -OutFile ($path+'.partial') -UseBasicParsing
    if ((Get-FileHash -LiteralPath ($path+'.partial') -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {{ throw 'hash mismatch' }}
    Move-Item -LiteralPath ($path+'.partial') -Destination $path
  }}
  if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {{ throw 'existing hash mismatch' }}
  @{{sha256=$expected;bytes=(Get-Item -LiteralPath $path).Length;reused=$reused}} | ConvertTo-Json -Compress
}} catch {{
  Write-Output 'Archive delivery failed; inspect host and renew URL before retry'
  exit 1
}}
"""
    return ('powershell.exe -NoProfile -EncodedCommand '
            + base64.b64encode(script.encode('utf-16le')).decode()
            + '; exit $LASTEXITCODE')


def persist(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def wait_command(region, instance, command):
    for _ in range(600):
        result = aws(region, 'ssm', 'get-command-invocation', '--command-id', command,
                     '--instance-id', instance, invocation=True)
        if result and result['Status'] in ('Success', 'Failed', 'Cancelled', 'TimedOut'):
            return result
        time.sleep(2)
    raise RuntimeError(f'SSM command {command} is not terminal; resume this ledger to observe it')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--instance', required=True)
    parser.add_argument('--region', required=True)
    parser.add_argument('--root', required=True)
    args = parser.parse_args()
    plan_bytes = args.plan.read_bytes()
    plan = json.loads(plan_bytes)
    if plan.get('pending'):
        raise ValueError('deployment plan still has pending artifacts')
    identity = {'plan_sha256': hashlib.sha256(plan_bytes).hexdigest(),
                'repository': args.repository, 'instance': args.instance,
                'region': args.region, 'root': args.root}
    ledger = json.loads(args.output.read_text()) if args.output.exists() else {'identity': identity, 'archives': {}}
    if ledger['identity'] != identity:
        raise ValueError('delivery inputs changed; use a new ledger')
    for entry in plan['archives']:
        if 'artifact_id' not in entry:
            continue
        key = str(entry['artifact_id'])
        history = ledger['archives'].setdefault(key, [])
        while not history or history[-1].get('status') != 'Success':
            if not history or history[-1].get('status') in ('Failed', 'Cancelled', 'TimedOut'):
                if len(history) >= 2:
                    raise RuntimeError(f'Artifact {key} failed twice; inspect delivery evidence')
                metadata = json.loads(subprocess.check_output(
                    ['gh', 'api', f'repos/{args.repository}/actions/artifacts/{key}'], text=True))
                if metadata['expired'] or metadata['digest'] != 'sha256:' + entry['sha256']:
                    raise ValueError(f'Artifact {key} metadata differs from reviewed plan')
                url = artifact_download_url(args.repository, int(key))
                command = aws(args.region, 'ssm', 'send-command', '--instance-ids', args.instance,
                              '--document-name', 'AWS-RunPowerShellScript', '--timeout-seconds', '120',
                              '--parameters', json.dumps({'commands': [download_command(args.root, entry, url)],
                                                          'executionTimeout': ['600']}))['Command']['CommandId']
                history.append({'command_id': command})
                persist(args.output, ledger)
            result = wait_command(args.region, args.instance, history[-1]['command_id'])
            history[-1].update(status=result['Status'], response_code=result['ResponseCode'])
            if result['Status'] == 'Success':
                receipt = json.loads(result['StandardOutputContent'])
                if receipt['sha256'] != entry['sha256'] or result['ResponseCode'] != 0:
                    raise ValueError(f'Artifact {key} delivery receipt differs')
                history[-1]['receipt'] = receipt
            persist(args.output, ledger)
        print(json.dumps({'artifact': key, 'verified': True}), flush=True)


if __name__ == '__main__':
    main()
