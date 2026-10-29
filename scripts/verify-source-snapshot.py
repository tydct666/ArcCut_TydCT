"""Compare release/source ZIP contents to independently hashed source and stage files."""
import hashlib
import json
import pathlib
import stat
import sys
import zipfile

MAX_FILES = 20000
MAX_BYTES = 512 * 1024 * 1024


def safe_name(name):
    if not name or name.startswith('/') or '\\' in name or ':' in name or any(
        part in ('', '.', '..') for part in name.rstrip('/').split('/')
    ):
        raise ValueError('Unsafe archive path: ' + name)
    return name.rstrip('/')


def zip_inventory(archive, prefix=''):
    result, seen, total = {}, set(), 0
    with zipfile.ZipFile(archive) as zipped:
        for entry in zipped.infolist():
            name = safe_name(entry.filename)
            if name.casefold() in seen:
                raise ValueError('Duplicate archive path: ' + name)
            seen.add(name.casefold())
            if len(seen) > MAX_FILES:
                raise ValueError('Too many archive entries')
            if entry.flag_bits & 1 or stat.S_ISLNK(entry.external_attr >> 16):
                raise ValueError('Encrypted or linked archive entry: ' + name)
            if prefix:
                if entry.is_dir() and name == prefix.rstrip('/'):
                    continue
                if not name.startswith(prefix):
                    raise ValueError('Unexpected archive root: ' + name)
                name = name[len(prefix):]
            if entry.is_dir():
                continue
            total += entry.file_size
            if total > MAX_BYTES:
                raise ValueError('Expanded archive limit exceeded')
            digest, size = hashlib.sha256(), 0
            with zipped.open(entry) as source:
                while chunk := source.read(65536):
                    size += len(chunk)
                    if size > entry.file_size:
                        raise ValueError('Archive entry size mismatch: ' + name)
                    digest.update(chunk)
            if size != entry.file_size:
                raise ValueError('Archive entry size mismatch: ' + name)
            result[name] = {'bytes': size, 'sha256': digest.hexdigest()}
    return result


def directory_inventory(directory, exclude_root_git=False):
    result, total = {}, 0
    directory = pathlib.Path(directory)
    if not directory.is_dir() or directory.is_symlink() or getattr(directory, 'is_junction', lambda: False)():
        raise ValueError('Missing or linked upload directory')
    pending = list(directory.iterdir())
    while pending:
        item = pending.pop()
        # The upload mirror may already be a Git checkout. Metadata is local;
        # source archives and package inventories still reject every extra file.
        if exclude_root_git and item.parent == directory and item.name == '.git':
            continue
        if item.is_symlink() or getattr(item, 'is_junction', lambda: False)():
            raise ValueError('Linked upload entry')
        if item.is_dir():
            pending.extend(item.iterdir())
            continue
        if not item.is_file():
            continue
        total += item.stat().st_size
        if len(result) >= MAX_FILES or total > MAX_BYTES:
            raise ValueError('Upload directory limit exceeded')
        digest = hashlib.sha256()
        with item.open('rb') as stream:
            while chunk := stream.read(65536):
                digest.update(chunk)
        result[item.relative_to(directory).as_posix()] = {
            'bytes': item.stat().st_size, 'sha256': digest.hexdigest()
        }
    return result


def expected_inventory(files):
    result = {f['path']: {'bytes': f['bytes'], 'sha256': f['sha256']} for f in files}
    if len(result) != len(files):
        raise ValueError('Duplicate expected file')
    return result


def compare(actual, expected, label):
    if actual.keys() != expected.keys():
        missing = sorted(expected.keys() - actual.keys())
        extra = sorted(actual.keys() - expected.keys())
        raise ValueError(f'{label} inventory mismatch; missing={missing[:5]}, extra={extra[:5]}')
    for name, identity in expected.items():
        if actual[name] != identity:
            raise ValueError(f'{label} content mismatch: {name}')


def verify(stage, release_zip, source_zip, upload, expected):
    stage, source_zip = pathlib.Path(stage), pathlib.Path(source_zip)
    source_files = expected_inventory(expected['sourceFiles'])
    package_files = expected_inventory(expected['packageFiles'])
    embedded = stage / 'sources' / source_zip.name
    compare(directory_inventory(upload, exclude_root_git=True), source_files, 'Upload directory')
    compare(zip_inventory(source_zip), source_files, 'Standalone source ZIP')
    compare(zip_inventory(embedded), source_files, 'Embedded source ZIP')
    compare(zip_inventory(release_zip, stage.name + '/'), package_files, 'Release ZIP')
    # The packager copies the exact source archive; differing metadata is stale too.
    def archive_hash(file):
        with file.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').digest()
    if archive_hash(embedded) != archive_hash(source_zip):
        raise ValueError('Standalone and embedded source ZIP bytes differ')
    return {'status': 'passed', 'sourceFiles': len(source_files),
            'sourceSnapshot': expected['snapshot'], 'copiesCompared': 4}


if __name__ == '__main__':
    # Node sends UTF-8 JSON through a pipe. Windows' locale may otherwise decode
    # that pipe as GBK even though the filesystem paths are already Unicode.
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    try:
        expected = json.loads(sys.stdin.buffer.read().decode('utf-8'))
        print(json.dumps(verify(*sys.argv[1:5], expected), ensure_ascii=True))
    except (ValueError, OSError, zipfile.BadZipFile) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
