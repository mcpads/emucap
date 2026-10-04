#!/usr/bin/env python3
"""Package a clean CI native build, its runtime dependencies and matching sources."""
import argparse
import hashlib
import io
import json
import os
import plistlib
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


def run(*args):
    return subprocess.check_output([str(a) for a in args], text=True).strip()


def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def one(root, pattern):
    matches = sorted(p for p in root.glob(pattern) if p.exists())
    paths = list({p.resolve(): p for p in reversed(matches)}.values())
    if len(paths) != 1:
        raise ValueError(f'expected one {pattern} under {root}, found {len(paths)}')
    return paths[0]


def app_executable(app):
    with (app / 'Contents/Info.plist').open('rb') as f:
        name = plistlib.load(f)['CFBundleExecutable']
    return app / 'Contents/MacOS' / name


def select_outputs(root, adapter, mac):
    work = root / 'adapters' / adapter / 'work'
    names = {'mesen2': 'mesen', 'dolphin': 'dolphin-src', 'ppsspp': 'ppsspp',
             'mednafen': 'mednafen', 'flycast': 'work', 'np2kai': 'np2kai',
             'mame-pc98': 'mame-src', 'mame-neogeo': 'mame-src', 'pcsx2': 'pcsx2',
             'desmume-nds': 'src', 'xemu': 'xemu'}
    src = work / names[adapter] if adapter in names else next(p for p in work.glob(adapter + '-*') if p.is_dir())
    if adapter == 'mesen2':
        binary = one(src, 'bin/**/publish/Mesen.app/Contents/MacOS/Mesen' if mac else 'bin/**/publish/Mesen')
        # The managed launcher already uses this portable runtime directory.
        # Keep the mutable build sidecar outside an app resource seal.
        output = binary.parent
    elif adapter == 'dolphin':
        output = src / 'build-emucap-headless/Binaries'
        binary = output / 'dolphin-emu-nogui'
    elif adapter == 'ppsspp':
        binary = src / 'build-headless/PPSSPPHeadless'
        output = binary
    elif adapter == 'mednafen':
        binary = src / 'src/mednafen'
        output = binary
    elif adapter == 'flycast':
        apps = list((src / 'build').glob('*.app')) if mac else []
        output = apps[0] if apps else src / 'build/flycast'
        binary = app_executable(output) if apps else output
    elif adapter == 'np2kai':
        binary = src / ('sdl/np2kai_libretro.dylib' if mac else 'sdl/np2kai_libretro.so')
        output = binary
    elif adapter == 'mupen64plus':
        output = src / 'test'
        binary = one(output, 'libmupen64plus.dylib' if mac else 'libmupen64plus.so*')
    elif adapter == 'openmsx':
        output = one(src, 'derived/**/openMSX.app') if mac else src / 'install'
        binary = output / ('Contents/MacOS/openmsx' if mac else 'bin/openmsx')
    elif adapter in ('mame-pc98', 'mame-neogeo'):
        binary = (work / 'mame.raw').resolve()
        output = binary
    elif adapter == 'pcsx2':
        output = one(src, 'build-emucap/bin/*.app') if mac else src / 'build-emucap/bin'
        binary = app_executable(output) if mac else output / 'pcsx2-qt'
    elif adapter == 'desmume-nds':
        binary = src / 'desmume/src/frontend/posix/build-headless/cli/desmume-cli'
        output = binary
    else:
        output = src / 'dist'
        binary = output / ('xemu.app/Contents/MacOS/xemu' if mac else 'xemu')
    if not binary.is_file():
        raise ValueError(f'missing executable: {binary}')
    return work, src, output, binary


def copy(source, target):
    if source.is_dir():
        shutil.copytree(source, target, symlinks=True,
                        ignore=shutil.ignore_patterns('*.pdb', '*.o', '*.obj', '*.a', '*.lib', '*.log'))
    else:
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def format_of(path):
    if not path.is_file() or path.is_symlink():
        return None
    with path.open('rb') as f:
        magic = f.read(4)
    if magic == b'\x7fELF':
        return 'elf'
    if magic in (b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'):
        return 'macho'
    return None


def copy_library(source, target, origins):
    original = sha(source)
    if target.exists():
        if origins.get(target.resolve(), sha(target)) != original:
            raise ValueError(f'dependency collision: {source.name}')
        return False
    copy(source, target)
    origins[target.resolve()] = original
    return True


def linux_dependencies(stage):
    lib = stage / 'lib'
    lib.mkdir(exist_ok=True)
    queue = [p for p in stage.rglob('*') if format_of(p) == 'elf']
    seen = set()
    origins = {p.resolve(): sha(p) for p in queue}
    while queue:
        binary = queue.pop()
        if binary in seen:
            continue
        seen.add(binary)
        result = subprocess.run(['ldd', str(binary)], text=True, capture_output=True)
        if 'not found' in result.stdout:
            raise ValueError(f'unresolved dependency: {binary}\n{result.stdout}')
        for line in result.stdout.splitlines():
            match = re.search(r'=>\s+(/\S+)', line)
            if not match:
                continue
            dep = Path(match[1])
            if re.fullmatch(r'(libc|libm|libpthread|libdl|librt|libresolv|libutil)\.so\..*', dep.name):
                continue
            if dep.is_relative_to(stage):
                continue
            target = lib / dep.name
            if copy_library(dep, target, origins):
                queue.append(target)
        rel = os.path.relpath(lib, binary.parent)
        # Static ELF files have no dynamic section and need no relocation.
        dynamic = subprocess.run(['patchelf', '--print-rpath', str(binary)], capture_output=True)
        if dynamic.returncode == 0:
            subprocess.run(['patchelf', '--set-rpath', '$ORIGIN/' + rel, str(binary)], check=True)
    return seen


def macho_dependencies(stage, executable):
    lib = stage / 'lib'
    lib.mkdir(exist_ok=True)
    queue = [p for p in stage.rglob('*') if format_of(p) == 'macho']
    seen = set()
    origins = {p.resolve(): sha(p) for p in queue}
    source_paths = {}
    while queue:
        binary = queue.pop()
        if binary in seen:
            continue
        seen.add(binary)
        original = source_paths.get(binary.resolve(), binary)
        # SDL2-compat opens SDL3 dynamically, outside the Mach-O load commands.
        if binary.name.startswith('libSDL2') and b'@loader_path/libSDL3.dylib' in binary.read_bytes():
            sdl3 = Path(run('pkg-config', '--variable=libdir', 'sdl3')) / 'libSDL3.dylib'
            target = binary.parent / 'libSDL3.dylib'
            if copy_library(sdl3, target, origins):
                source_paths[target.resolve()] = sdl3.resolve()
                queue.append(target)
        details = run('otool', '-l', binary)
        rpaths = re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset', details)
        owner = next((p for p in binary.parents if p.suffix == '.app'), None)
        entry = app_executable(owner) if owner else executable
        if entry != binary:
            rpaths += re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset', run('otool', '-l', entry))
        for dep in [line.strip().split(' (')[0] for line in run('otool', '-L', binary).splitlines()[1:]]:
            if dep.startswith(('/System/Library/', '/usr/lib/')):
                continue
            def expand(value):
                return value.replace('@loader_path', str(binary.parent)).replace('@executable_path', str(entry.parent))
            candidates = [Path(expand(dep))]
            if dep.startswith('@loader_path/'):
                candidates.append(original.parent / dep[len('@loader_path/'):])
            if dep.startswith('@rpath/'):
                candidates = [Path(expand(r)) / dep[7:] for r in rpaths]
                candidates += [stage / 'lib' / Path(dep).name, Path('/opt/homebrew/lib') / dep[7:]]
            found = next((p.resolve() for p in candidates if p.is_file()), None)
            # A dylib's install name is listed by otool alongside its dependencies.
            identity = subprocess.run(['otool', '-D', str(binary)], capture_output=True, text=True).stdout.splitlines()[1:]
            if dep in identity:
                continue
            if found is None:
                raise ValueError(f'unresolved Mach-O dependency {dep} in {binary}')
            if found.is_relative_to(stage):
                target = found
            else:
                framework = next((p for p in found.parents if p.suffix == '.framework'), None)
                if framework:
                    dest = lib / framework.name
                    if not dest.exists():
                        copy(framework, dest)
                        members = [p for p in dest.rglob('*') if format_of(p) == 'macho']
                        origins.update({p.resolve(): sha(p) for p in members})
                        source_paths.update({p.resolve(): framework / p.relative_to(dest) for p in members})
                        queue += members
                    target = dest / found.relative_to(framework)
                else:
                    target = lib / found.name
                    if copy_library(found, target, origins):
                        source_paths[target.resolve()] = found
                        queue.append(target)
            rewritten = '@loader_path/' + os.path.relpath(target, binary.parent)
            if dep != rewritten:
                subprocess.run(['install_name_tool', '-change', dep, rewritten, str(binary)], check=True)
    apps = sorted(stage.rglob('*.app'), key=lambda p: len(p.parts), reverse=True)
    app_mains = {app_executable(app).resolve() for app in apps}
    for binary in sorted(seen):
        # Signing a bundle main executable validates the whole bundle. Defer it
        # until nested code has been signed, then sign through the bundle path.
        if binary.resolve() in app_mains:
            continue
        command = ['codesign', '--force', '--sign', '-']
        if subprocess.run(['codesign', '-dv', str(binary)], capture_output=True).returncode == 0:
            command += ['--preserve-metadata=entitlements,requirements,flags,runtime']
        subprocess.run([*command, str(binary)], check=True)
    for app in apps:
        subprocess.run(['codesign', '--force', '--deep', '--sign', '-',
                        '--preserve-metadata=entitlements,requirements,flags,runtime', str(app)], check=True)
    return seen


def files_manifest(stage):
    stage = stage.resolve()
    result = {}
    for p in sorted(stage.rglob('*')):
        if p.is_symlink():
            if not p.resolve().is_relative_to(stage):
                raise ValueError(f'escaping package symlink: {p}')
            result[p.relative_to(stage).as_posix()] = {'symlink': os.readlink(p)}
        elif p.is_file():
            result[p.relative_to(stage).as_posix()] = {'sha256': sha(p), 'bytes': p.stat().st_size}
    return result


def source_archive(source, src, adapter, output, name):
    excluded = {'.git', '.deps', '.libs', '__pycache__', 'CMakeFiles', 'node_modules'}
    build_dirs = {'mesen2': {'bin', 'obj'}, 'dolphin': {'build-emucap-headless', 'build-emucap-gui'},
                  'ppsspp': {'build-headless'}, 'flycast': {'build'}, 'mupen64plus': {'test'},
                  'openmsx': {'derived', 'install'}, 'mame-pc98': {'build'}, 'mame-neogeo': {'build'},
                  'pcsx2': {'build-emucap'}, 'xemu': {'build', 'dist', 'macos-libs'}}.get(adapter, set())
    external_links = {}
    def include(info):
        rel = Path(info.name).parts[2:]
        if any(p in excluded for p in rel) or (rel and rel[0] in build_dirs):
            return None
        if info.issym():
            target = (src.joinpath(*rel).parent / info.linkname).resolve()
            if not target.is_relative_to(src.resolve()):
                # Preserve the upstream reference without exporting host files or
                # creating an unsafe link during source archive extraction.
                external_links['/'.join(rel)] = info.linkname
                return None
        if info.isfile() and Path(info.name).suffix in ('.o', '.obj', '.pdb', '.exe', '.dll', '.dylib', '.so', '.a'):
            return None
        return info
    path = output / (name + '-source.tar.gz')
    with tarfile.open(path, 'w:gz', compresslevel=3) as archive:
        archive.add(src, arcname=name + '-source/upstream', filter=include)
        if external_links:
            data = (json.dumps(external_links, indent=2) + '\n').encode()
            info = tarfile.TarInfo(name + '-source/EXTERNAL-SYMLINKS.json')
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
            data = ("EXTERNAL-SYMLINKS.json records upstream symlinks whose targets are outside "
                    "the source tree. They are not followed or created in this archive. Paths "
                    "are relative to upstream/. Restore a listed link when building its component "
                    "against the corresponding system headers.\n").encode()
            info = tarfile.TarInfo(name + '-source/SOURCE-LAYOUT.txt')
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        tracked = run('git', '-C', source, 'ls-files', 'adapters/' + adapter, 'adapters/_common',
                      'LICENSE', 'NOTICE', 'licenses').splitlines()
        if adapter == 'mame-neogeo':
            tracked += run('git', '-C', source, 'ls-files', 'adapters/mame-pc98').splitlines()
        for rel in sorted(set(tracked)):
            archive.add(source / rel, arcname=name + '-source/recipes/' + rel, recursive=False)
        archive.add(Path(__file__).with_name('native_unix_build.sh'), arcname=name + '-source/native_unix_build.sh')
    return path


def move_app_metadata(stage):
    # Build metadata binds the signed executable digest and must not be part of
    # that executable's resource seal. Keep legacy recipe output unchanged.
    for metadata in list(stage.rglob('*emucap*build.json')):
        app = next((p for p in metadata.parents if p.suffix == '.app'), None)
        if app is None:
            continue
        target = app.parent / metadata.name
        if target.exists() and sha(target) != sha(metadata):
            raise ValueError(f'conflicting app metadata: {metadata.name}')
        shutil.copy2(metadata, target)
        metadata.unlink()


def restore_package(directory, name, revision, stage):
    stage = stage.resolve()
    record = json.loads((directory / (name + '.json')).read_text())
    if record['source_revision'] != revision:
        raise ValueError('reused package producer revision mismatch')
    for filename in (name + '.tar.gz', name + '-source.tar.gz'):
        if record['artifacts'].get(filename) != sha(directory / filename):
            raise ValueError(f'reused package digest mismatch: {filename}')
    with tarfile.open(directory / (name + '.tar.gz')) as archive:
        archive.extractall(stage.parent, filter='data')
    manifest = json.loads((stage / 'NATIVE-PACKAGE.json').read_text())
    actual = files_manifest(stage)
    del actual['NATIVE-PACKAGE.json']
    if actual != manifest['files']:
        raise ValueError('reused package file manifest mismatch')
    if any(manifest[k] != record[k] for k in ('adapter', 'target', 'source_revision', 'version', 'executable')):
        raise ValueError('reused package identity mismatch')
    executable = stage / manifest['executable']
    if not executable.resolve().is_relative_to(stage) or not executable.is_file():
        raise ValueError('reused package executable escapes package')
    (stage / 'NATIVE-PACKAGE.json').unlink()
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--adapter', required=True)
    parser.add_argument('--platform', choices=['linux', 'macos'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--reuse', type=Path, help='Verified complete package directory to repackage without compilation')
    args = parser.parse_args()
    source = args.source.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    version = tomllib.loads((source / 'Cargo.toml').read_text())['package']['version']
    revision = run('git', '-C', source, 'rev-parse', 'HEAD')
    mac = args.platform == 'macos'
    target = 'x86_64-apple-darwin' if mac and args.adapter == 'pcsx2' else ('aarch64-apple-darwin' if mac else 'x86_64-unknown-linux-gnu')
    name = f'emucap-{version}-{args.adapter}-{target}'
    if not args.reuse:
        work, src, original, binary = select_outputs(source, args.adapter, mac)
    with tempfile.TemporaryDirectory(prefix='emucap-native-') as temporary:
        stage = Path(temporary).resolve() / name
        if args.reuse:
            executable = restore_package(args.reuse.resolve(), name, revision, stage)
        else:
            stage.mkdir()
            dest = stage / original.name if original.suffix == '.app' or original.is_file() else stage / 'runtime'
            copy(original, dest)
            executable = dest / binary.relative_to(original) if original.is_dir() else dest
            # Build sidecars may sit beside a standalone executable or outside its app.
            for parent in (binary.parent, original.parent, work, src):
                for metadata in parent.glob('*emucap*build.json'):
                    target_meta = executable.parent / metadata.name
                    if not target_meta.exists():
                        shutil.copy2(metadata, target_meta)
            if args.adapter == 'openmsx' and not mac:
                metadata = one(src, 'derived/**/emucap-openmsx-build.json')
                shutil.copy2(metadata, executable.parent / metadata.name)
            if args.adapter == 'dolphin':
                gui = src / 'build-emucap-gui/Binaries'
                if gui.is_dir(): copy(gui, stage / 'gui')
            if args.adapter == 'ppsspp':
                for extra in ('assets', 'PPSSPPSDL', 'PPSSPPSDL.app'):
                    p = binary.parent / extra
                    if p.exists(): copy(p, stage / extra)
            if args.adapter.startswith('mame-'):
                copy(src / 'hash', stage / 'hash')
        move_app_metadata(stage)
        native_files = macho_dependencies(stage, executable) if mac else linux_dependencies(stage)
        for p in native_files:
            arches = run('lipo', '-archs', p) if mac else run('file', p)
            expected = 'x86_64' if mac and args.adapter == 'pcsx2' else ('arm64' if mac else 'x86-64')
            if expected not in arches:
                raise ValueError(f'wrong architecture in {p}: {arches}')
        for p in stage.rglob('*emucap*build.json'):
            value = json.loads(p.read_text())
            if 'binary_sha256' in value:
                value['binary_sha256'] = sha(executable)
                p.write_text(json.dumps(value, indent=2) + '\n')
        if not args.reuse:
            notices = stage / 'licenses'
            notices.mkdir()
            copy(source / 'NOTICE', notices / 'EMUCAP-NOTICE')
            for p in src.iterdir():
                if p.is_file() and any(word in p.name.lower() for word in ('license', 'copying', 'copyright', 'notice')):
                    copy(p, notices / p.name)
        manifest = {'adapter': args.adapter, 'target': target, 'source_revision': revision,
                    'version': version, 'executable': executable.relative_to(stage).as_posix(),
                    'packaging_revision': run('git', '-C', Path(__file__).resolve().parent, 'rev-parse', 'HEAD'),
                    'files': files_manifest(stage)}
        if args.reuse:
            manifest['repackaged_from_sha256'] = sha(args.reuse / (name + '.tar.gz'))
        (stage / 'NATIVE-PACKAGE.json').write_text(json.dumps(manifest, indent=2) + '\n')
        archive_path = output / (name + '.tar.gz')
        with tarfile.open(archive_path, 'w:gz', compresslevel=3) as archive:
            archive.add(stage, arcname=name)
        verify = Path(temporary) / 'verify'
        with tarfile.open(archive_path) as archive:
            archive.extractall(verify, filter='data')
        if mac:
            for binary in native_files:
                restored_binary = verify / name / binary.relative_to(stage)
                subprocess.run(['codesign', '--verify', str(restored_binary)], check=True)
                if restored_binary.name.startswith('libSDL2') and b'@loader_path/libSDL3.dylib' in restored_binary.read_bytes():
                    subprocess.run([sys.executable, '-c', 'import ctypes,sys; ctypes.CDLL(sys.argv[1])',
                                    str(restored_binary)], check=True, timeout=30)
            for app in (verify / name).rglob('*.app'):
                subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
        if args.adapter == 'openmsx':
            home = Path(temporary) / 'smoke-home'
            home.mkdir()
            env = dict(os.environ, HOME=str(home), OPENMSX_HOME=str(home),
                       OPENMSX_USER_DATA=str(home / 'share'))
            if not mac:
                env['OPENMSX_SYSTEM_DATA'] = str(verify / name / 'runtime/share')
            subprocess.run([str(verify / name / executable.relative_to(stage)), '-testconfig'],
                           env=env, stdin=subprocess.DEVNULL, check=True, timeout=30)
        restored = files_manifest(verify / name)
        del restored['NATIVE-PACKAGE.json']
        assert restored == manifest['files'], 'extracted native package mismatch'
        if args.reuse:
            sources = output / (name + '-source.tar.gz')
            shutil.copy2(args.reuse / sources.name, sources)
        else:
            sources = source_archive(source, src, args.adapter, output, name)
        record = {**{k: v for k, v in manifest.items() if k != 'files'},
                  'artifacts': {p.name: sha(p) for p in (archive_path, sources)}}
        (output / (name + '.json')).write_text(json.dumps(record, indent=2) + '\n')
        (output / (name + '.sha256')).write_text(''.join(f'{digest}  {path}\n' for path, digest in record['artifacts'].items()))
        print(json.dumps(record))


if __name__ == '__main__':
    main()
