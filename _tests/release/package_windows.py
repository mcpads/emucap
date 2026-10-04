"""Package an authenticated Windows CI payload without rebuilding native code."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

from native_build import OUTPUTS, payload_hashes
from package_native import files_manifest, run, sha


def verify_payload(payload, source, adapter, binary_digest=None):
    metadata = payload / 'NATIVE-BUILD.json'
    revision = run('git', '-C', source, 'rev-parse', 'HEAD')
    if metadata.exists():
        record = json.loads(metadata.read_text())
        if record['files'] != payload_hashes(payload) or not record['files']:
            raise ValueError('Windows payload file manifest mismatch')
        if 'build_inputs' in record:
            inputs = record['build_inputs']
            encoded = json.dumps(inputs, sort_keys=True, separators=(',', ':')).encode()
            if hashlib.sha256(encoded).hexdigest() != record['identity'] or inputs['adapter'] != adapter:
                raise ValueError('Windows build identity mismatch')
            for name, digest in inputs['inputs'].items():
                path = source / name
                if not path.resolve().is_relative_to(source.resolve()) or sha(path) != digest:
                    raise ValueError(f'Windows producer input mismatch: {name}')
            binary = OUTPUTS[adapter][1]
        else:
            if record['revision'] != revision or record['adapter'] != adapter:
                raise ValueError('Windows producer revision mismatch')
            binary = record['executable']
    elif adapter == 'xemu' and binary_digest:
        # The cross-build records the executable hash in its separate evidence artifact.
        binary = 'xemu.exe'
        if sha(payload / binary) != binary_digest:
            raise ValueError('Windows cross-build binary digest mismatch')
    else:
        raise ValueError('Windows payload lacks build evidence')
    executable = payload / binary
    if not executable.resolve().is_relative_to(payload.resolve()) or not executable.is_file():
        raise ValueError('Windows executable escapes payload')
    return binary


def matching_source(directory, source, adapter):
    records = [json.loads(p.read_text()) for p in directory.glob('emucap-*.json')]
    records = [r for r in records if r.get('adapter') == adapter and 'artifacts' in r]
    if len(records) != 1:
        raise ValueError('expected one matching native source record')
    record = records[0]
    names = [n for n in record['artifacts'] if n.endswith('-source.tar.gz')]
    if len(names) != 1 or Path(names[0]).name != names[0]:
        raise ValueError('invalid native source artifact name')
    archive = directory / names[0]
    if sha(archive) != record['artifacts'][archive.name]:
        raise ValueError('native source archive digest mismatch')
    owner = 'mame-pc98' if adapter == 'mame-neogeo' else adapter
    tracked = run('git', '-C', source, 'ls-files', f'adapters/{owner}', f'adapters/{adapter}').splitlines()
    # Native code can be copied into upstream directly, not only applied as patches.
    # Platform build recipes are supplied separately from the exact Windows commit.
    required = [p for p in tracked if Path(p).suffix not in ('.md', '.sh', '.ps1')]
    if f'adapters/{owner}/upstream.lock' not in required:
        raise ValueError('missing upstream lock')
    with tarfile.open(archive) as packed:
        for rel in required:
            member = f'{archive.name[:-7]}/recipes/{rel}'
            data = packed.extractfile(member)
            if data is None or data.read() != (source / rel).read_bytes():
                raise ValueError(f'native source input mismatch: {rel}')
    return archive, record


def package(args):
    source = args.source.resolve()
    binary = verify_payload(args.payload, source, args.adapter, args.binary_digest)
    sources, source_record = matching_source(args.native_source, source, args.adapter)
    revision = run('git', '-C', source, 'rev-parse', 'HEAD')
    version = tomllib.loads((source / 'Cargo.toml').read_text())['package']['version']
    name = f'emucap-{version}-{args.adapter}-windows-x86_64'
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temporary:
        stage = Path(temporary) / name
        shutil.copytree(args.payload, stage)
        licenses = stage / 'licenses'
        licenses.mkdir(exist_ok=True)
        shutil.copy2(source / 'NOTICE', licenses / 'EMUCAP-NOTICE')
        # Keep upstream notices, including bundled component notices, at their source paths.
        with tarfile.open(sources) as archive:
            for member in archive:
                parts = PurePosixPath(member.name).parts
                if len(parts) < 3 or parts[1] != 'upstream' or not member.isfile():
                    continue
                if any(word in parts[-1].lower() for word in ('license', 'copying', 'copyright', 'notice')):
                    path = licenses / Path(*parts[2:])
                    if not path.resolve().is_relative_to(licenses.resolve()):
                        raise ValueError('notice path escapes package')
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(archive.extractfile(member).read())
        manifest = dict(adapter=args.adapter, target='windows-x86_64', version=version,
                        source_revision=revision, executable=binary,
                        source_archive_revision=source_record['source_revision'],
                        packaging_revision=run('git', '-C', Path(__file__).parent, 'rev-parse', 'HEAD'),
                        files=files_manifest(stage))
        (stage / 'NATIVE-PACKAGE.json').write_text(json.dumps(manifest, indent=2) + '\n')
        target = args.output / (name + '.zip')
        with zipfile.ZipFile(target, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for path in sorted(stage.rglob('*')):
                if path.is_file(): archive.write(path, path.relative_to(stage.parent))
        restored = Path(temporary) / 'verify'
        with zipfile.ZipFile(target) as archive: archive.extractall(restored)
        actual = files_manifest(restored / name)
        del actual['NATIVE-PACKAGE.json']
        if actual != manifest['files']:
            raise ValueError('extracted Windows package mismatch')
    # Preserve the verified upstream source archive verbatim. A separate archive supplies
    # the exact Windows producer recipes, which may differ from its Unix source collector.
    shutil.copy2(sources, args.output / sources.name)
    recipes = args.output / (name + '-recipes.tar.gz')
    paths = [f'adapters/{args.adapter}', 'adapters/_common', 'LICENSE', 'NOTICE', 'licenses']
    if args.adapter == 'mame-neogeo': paths.append('adapters/mame-pc98')
    subprocess.run(['git', '-C', str(source), 'archive', '--format=tar.gz',
                    f'--output={recipes.resolve()}', 'HEAD', '--', *paths], check=True)
    record = {k: v for k, v in manifest.items() if k != 'files'}
    record['artifacts'] = {p.name: sha(p) for p in (target, args.output / sources.name, recipes)}
    (args.output / (name + '.json')).write_text(json.dumps(record, indent=2) + '\n')
    (args.output / (name + '.sha256')).write_text(''.join(f'{digest}  {path}\n' for path, digest in record['artifacts'].items()))
    print(json.dumps(record))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('source', 'payload', 'native-source', 'output'):
        parser.add_argument('--' + key, type=Path, required=True)
    parser.add_argument('--adapter', required=True)
    parser.add_argument('--binary-digest')
    package(parser.parse_args())


if __name__ == '__main__': main()
