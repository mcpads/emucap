#!/usr/bin/env python3
"""Inspect an Actions ZIP's directory and native manifest using bounded range reads.

No bulk-download fallback. The signed download URL and GitHub token are not
written to the result. Complete archive digest verification belongs to deployment.
"""
import argparse
import io
import json
from pathlib import Path
import re
import subprocess
import urllib.error
import urllib.request
import zipfile


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class RemoteZip(io.RawIOBase):
    def __init__(self, url):
        self.url = url
        self.position = 0
        self.transferred = 0
        self.length = None
        self._range(0, 0)

    def _range(self, start, end):
        request = urllib.request.Request(self.url, headers={'Range': f'bytes={start}-{end}'})
        with urllib.request.urlopen(request, timeout=30) as response:
            if response.status != 206:
                raise ValueError('artifact server did not honor bounded Range request')
            match = re.fullmatch(r'bytes (\d+)-(\d+)/(\d+)', response.headers.get('Content-Range', ''))
            if not match or tuple(map(int, match.groups()[:2])) != (start, end):
                raise ValueError('unexpected artifact byte range')
            length = int(match[3])
            if self.length is not None and self.length != length:
                raise ValueError('artifact length changed')
            self.length = length
            data = response.read(end - start + 2)
        if len(data) != end - start + 1:
            raise ValueError('incomplete artifact byte range')
        self.transferred += len(data)
        return data

    def seek(self, offset, whence=0):
        self.position = (0 if whence == 0 else self.position if whence == 1 else self.length) + offset
        if self.position < 0:
            raise ValueError('negative artifact offset')
        return self.position

    def tell(self):
        return self.position

    def seekable(self):
        return True

    def read(self, size=-1):
        size = min(self.length - self.position, size if size >= 0 else self.length)
        if size <= 0:
            return b''
        if size > 2 * 1024 * 1024 or self.transferred + size > 8 * 1024 * 1024:
            raise ValueError('artifact metadata exceeds inspection byte budget')
        data = self._range(self.position, self.position + size - 1)
        self.position += len(data)
        return data


def artifact_download_url(repository, artifact_id):
    endpoint = f'repos/{repository}/actions/artifacts/{artifact_id}'
    token = subprocess.check_output(['gh', 'auth', 'token'], text=True).strip()
    request = urllib.request.Request('https://api.github.com/' + endpoint + '/zip',
                                     headers={'Authorization': 'Bearer ' + token})
    try:
        with urllib.request.build_opener(NoRedirect).open(request, timeout=30):
            raise ValueError('expected signed artifact redirect')
    except urllib.error.HTTPError as response:
        try:
            if response.code != 302:
                raise RuntimeError(f'GitHub artifact redirect failed: HTTP {response.code}') from None
            url = response.headers['Location']
        finally:
            response.close()
    return url


def inspect(repository, artifact_id):
    endpoint = f'repos/{repository}/actions/artifacts/{artifact_id}'
    metadata = json.loads(subprocess.check_output(['gh', 'api', endpoint], text=True))
    url = artifact_download_url(repository, artifact_id)
    # Authorization is used only for api.github.com, never for the signed blob URL.
    stream = RemoteZip(url)
    with zipfile.ZipFile(stream) as archive:
        members = [{'path': item.filename, 'bytes': item.file_size} for item in archive.infolist()]
        manifests = {}
        for item in archive.infolist():
            if item.filename.rsplit('/', 1)[-1] in ('NATIVE-BUILD.json', 'NATIVE-HOSTS.json'):
                if item.file_size > 2 * 1024 * 1024:
                    raise ValueError('native manifest exceeds inspection byte budget')
                manifests[item.filename] = json.loads(archive.read(item))
    return {'artifact': metadata, 'members': members, 'manifests': manifests,
            'transferred_bytes': stream.transferred, 'archive_digest_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--artifact', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = inspect(args.repository, args.artifact)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'artifact': args.artifact, 'members': len(result['members']),
                      'transferred_bytes': result['transferred_bytes']}))


if __name__ == '__main__':
    main()
