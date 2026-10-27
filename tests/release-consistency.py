"""Regression checks for cross-artifact consistency, using disposable ZIP fixtures."""
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('source_check', ROOT / 'scripts/verify-source-snapshot.py')
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class ConsistencyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='arkcut-source-consistency-')
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.upload = self.root / 'repo'
        self.upload.mkdir()
        (self.upload / 'main.ts').write_text('const version = 1;', encoding='utf8')
        self.source = self.root / 'ArkCut-1.0.0-source.zip'
        self.write_zip(self.source, {'main.ts': b'const version = 1;'})
        self.stage = self.root / 'ArkCut-1.0.0-win64-light'
        (self.stage / 'sources').mkdir(parents=True)
        self.embedded = self.stage / 'sources' / self.source.name
        shutil.copyfile(self.source, self.embedded)
        (self.stage / 'ArkCut.exe').write_bytes(b'fixture')
        self.release = self.root / 'release.zip'
        self.rebuild_release()
        source = CHECK.directory_inventory(self.upload)
        self.expected = {
            'sourceFiles': [dict(path=n, **f) for n, f in source.items()],
            'snapshot': hashlib.sha256(b'main.ts\0const version = 1;').hexdigest(),
            'packageFiles': [dict(path=n, **f) for n, f in CHECK.directory_inventory(self.stage).items()]
        }

    @staticmethod
    def write_zip(destination, entries, comment=b''):
        with zipfile.ZipFile(destination, 'w') as zipped:
            for name, data in entries.items():
                zipped.writestr(name, data)
            zipped.comment = comment

    def rebuild_release(self):
        self.write_zip(self.release, {
            self.stage.name + '/' + f.relative_to(self.stage).as_posix(): f.read_bytes()
            for f in self.stage.rglob('*') if f.is_file()
        })

    def verify(self):
        return CHECK.verify(self.stage, self.release, self.source, self.upload, self.expected)

    def test_coherent_artifacts(self):
        self.assertEqual(self.verify()['status'], 'passed')

    def test_utf8_pipe_with_chinese_paths_under_gbk_locale(self):
        name = 'docs/用户指南.md'
        guide = self.upload / name
        guide.parent.mkdir()
        guide.write_text('中文使用说明', encoding='utf8')
        self.write_zip(self.source, {
            f.relative_to(self.upload).as_posix(): f.read_bytes()
            for f in self.upload.rglob('*') if f.is_file()
        })
        shutil.copyfile(self.source, self.embedded)
        self.rebuild_release()
        self.expected['sourceFiles'] = [dict(path=n, **f) for n, f in CHECK.directory_inventory(self.upload).items()]
        self.expected['packageFiles'] = [dict(path=n, **f) for n, f in CHECK.directory_inventory(self.stage).items()]
        payload = json.dumps(self.expected, ensure_ascii=False).encode('utf-8')
        command = [sys.executable, str(ROOT / 'scripts/verify-source-snapshot.py'),
                   str(self.stage), str(self.release), str(self.source), str(self.upload)]
        environment = dict(os.environ, PYTHONIOENCODING='gbk', PYTHONUTF8='0')
        result = subprocess.run(command, input=payload, capture_output=True, env=environment)
        self.assertEqual(result.returncode, 0, result.stderr.decode('utf-8'))
        self.assertEqual(json.loads(result.stdout.decode('utf-8'))['status'], 'passed')
        # Diagnostics must also retain Chinese names when the caller decodes UTF-8.
        guide.write_text('已修改', encoding='utf8')
        failed = subprocess.run(command, input=payload, capture_output=True, env=environment)
        self.assertEqual(failed.returncode, 1)
        self.assertIn(name, failed.stderr.decode('utf-8'))

    def test_stale_upload(self):
        (self.upload / 'main.ts').write_text('const version = 0;', encoding='utf8')
        with self.assertRaisesRegex(ValueError, 'Upload directory content mismatch'):
            self.verify()

    def test_extra_upload_file(self):
        (self.upload / 'debug.log').write_text('extra', encoding='utf8')
        with self.assertRaisesRegex(ValueError, 'Upload directory inventory mismatch'):
            self.verify()

    def test_stale_standalone_source(self):
        self.write_zip(self.source, {'main.ts': b'const version = 0;'})
        with self.assertRaisesRegex(ValueError, 'Standalone source ZIP content mismatch'):
            self.verify()

    def test_stale_embedded_source(self):
        self.write_zip(self.embedded, {'main.ts': b'const version = 0;'})
        with self.assertRaisesRegex(ValueError, 'Embedded source ZIP content mismatch'):
            self.verify()

    def test_zip_differs_from_staging(self):
        self.write_zip(self.release, {
            self.stage.name + '/ArkCut.exe': b'old',
            self.stage.name + '/sources/' + self.source.name: self.embedded.read_bytes()
        })
        with self.assertRaisesRegex(ValueError, 'Release ZIP content mismatch'):
            self.verify()

    def test_unexpected_zip_root(self):
        self.write_zip(self.release, {'wrong/ArkCut.exe': b'fixture'})
        with self.assertRaisesRegex(ValueError, 'Unexpected archive root'):
            self.verify()

    def test_archive_traversal(self):
        self.write_zip(self.source, {'../main.ts': b'bad'})
        with self.assertRaisesRegex(ValueError, 'Unsafe archive path'):
            self.verify()

    def test_duplicate_archive_name(self):
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(self.source, 'a') as zipped:
                zipped.writestr('main.ts', b'const version = 1;')
        with self.assertRaisesRegex(ValueError, 'Duplicate archive path'):
            self.verify()

    def test_source_zip_metadata_must_match_too(self):
        self.write_zip(self.source, {'main.ts': b'const version = 1;'}, comment=b'different')
        with self.assertRaisesRegex(ValueError, 'source ZIP bytes differ'):
            self.verify()

    def test_manifest_and_signed_source_identity(self):
        script = """
const assert=require('assert');
const {verifySourceIdentity}=require('./scripts/verify-release.cjs');
const sourceSnapshot=require('./scripts/source-list.cjs').snapshot();
assert.equal(verifySourceIdentity({sourceSnapshot},{sourceCommit:sourceSnapshot}),sourceSnapshot);
assert.equal(verifySourceIdentity({sourceSnapshot,sourceCommit:'a'.repeat(40)},{sourceCommit:'a'.repeat(40)}),sourceSnapshot);
assert.throws(()=>verifySourceIdentity({sourceSnapshot:'0'.repeat(64)}),/snapshot differs/);
assert.throws(()=>verifySourceIdentity({sourceSnapshot},{sourceCommit:'a'.repeat(40)}),/identity differs/);
"""
        result = subprocess.run(['node', '-e', script], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
