#!/usr/bin/env python3
"""Offline bootstrap security/atomicity tests; never downloads or executes a real app."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('desktop_launch', Path(__file__).with_name('desktop-launch.py'))
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


class DesktopBootstrapTests(unittest.TestCase):
    def bundle(self, root, names=None, symlink=False):
        archive = root / 'bundle.tar.gz'
        with tarfile.open(archive, 'w:gz') as bundle:
            for name in names or sorted(launcher.BINARIES):
                member = tarfile.TarInfo(name)
                if symlink:
                    member.type = tarfile.SYMTYPE
                    member.linkname = '/etc/passwd'
                    bundle.addfile(member)
                else:
                    member.size = 4
                    bundle.addfile(member, io.BytesIO(b'test'))
        checksum = root / 'checksum'
        checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  bundle.tar.gz\n')
        return archive, checksum

    def test_extracts_only_verified_regular_executables(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive, checksum = self.bundle(root)
            output = root / 'output'
            output.mkdir()
            launcher.unpack_verified(archive, checksum, output)
            for name in launcher.BINARIES:
                self.assertEqual((output / name).read_bytes(), b'test')
                self.assertTrue((output / name).stat().st_mode & 0o100)

    def test_corruption_empty_checksum_traversal_and_links_are_refused(self):
        for attack in ('corrupt', 'empty', 'path', 'link'):
            with self.subTest(attack=attack), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                archive, checksum = self.bundle(root, ['../escape', 'holonomy-gui'] if attack == 'path' else None,
                                                symlink=attack == 'link')
                if attack == 'corrupt':
                    checksum.write_text('0' * 64)
                elif attack == 'empty':
                    checksum.write_text('')
                output = root / 'output'
                output.mkdir()
                with self.assertRaises(ValueError):
                    launcher.unpack_verified(archive, checksum, output)
                self.assertEqual(list(output.iterdir()), [])
                self.assertFalse((root / 'escape').exists())

    def test_install_is_source_matched_atomic_and_cached(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive, checksum = self.bundle(root)
            urls = []

            def download(url, destination, limit=launcher.MAX_DOWNLOAD):
                urls.append(url)
                destination.write_bytes((checksum if url.endswith('.sha256') else archive).read_bytes())

            revision = 'a' * 40
            with mock.patch.object(launcher, 'download', side_effect=download):
                cache = launcher.install(root, revision)
                self.assertEqual(launcher.install(root, revision), cache)
            self.assertEqual(len(urls), 2, 'second launch must not download or compile')
            self.assertTrue(all('/desktop-' + revision + '/' in url for url in urls))
            self.assertEqual({path.name for path in cache.iterdir()}, launcher.BINARIES)
            other_revision = 'b' * 40
            with mock.patch.object(launcher, 'download', side_effect=OSError('offline')):
                with self.assertRaises(OSError):
                    launcher.install(root, other_revision)
            self.assertFalse((cache.parent / other_revision).exists())
            self.assertTrue(all((cache / name).is_file() for name in launcher.BINARIES))
            self.assertEqual(sorted(path.name for path in cache.parent.iterdir()), ['.install.lock', revision])


if __name__ == '__main__':
    unittest.main()
