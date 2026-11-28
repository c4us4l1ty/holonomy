#!/usr/bin/env python3
"""Isolated launcher regression tests; no Rust toolchain or GUI is required."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class DesktopScriptTests(unittest.TestCase):
    def test_caller_paths_build_profiles_and_failure_propagation(self):
        with tempfile.TemporaryDirectory(prefix="holonomy launcher ") as temp:
            base = Path(temp).resolve()
            root = base / "source tree"
            (root / "scripts").mkdir(parents=True)
            script = root / "scripts/run-desktop.sh"
            shutil.copyfile(Path(__file__).with_name("run-desktop.sh"), script)
            (root / "Cargo.toml").touch()
            tools = base / "tools"
            tools.mkdir()
            cargo = tools / "cargo"
            cargo.write_text("#!/usr/bin/env python3\n"
                             "import json, os, sys\n"
                             "from pathlib import Path\n"
                             "Path(os.environ['BUILD_LOG']).write_text(json.dumps(sys.argv[1:]))\n"
                             "sys.exit(int(os.environ.get('BUILD_EXIT', '0')))\n")
            cargo.chmod(0o755)
            for profile in ("debug", "release"):
                folder = root / "target" / profile
                folder.mkdir(parents=True)
                binary = folder / "holonomy-gui"
                binary.write_text("#!/usr/bin/env python3\n"
                                  "import json, os, sys\n"
                                  "from pathlib import Path\n"
                                  "Path(os.environ['RUN_LOG']).write_text(json.dumps([os.getcwd(), sys.argv[1:]]))\n"
                                  "sys.exit(int(os.environ.get('APP_EXIT', '0')))\n")
                binary.chmod(0o755)
            cwd = base / "my documents"
            cwd.mkdir()
            build_log, run_log = base / "build.json", base / "run.json"
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                       BUILD_LOG=str(build_log), RUN_LOG=str(run_log))
            env.pop("HOLONOMY_BUILD_PROFILE", None)
            args = ["relative notes.wavefunction"]
            for profile in ("dev", "release"):
                env['HOLONOMY_BUILD_PROFILE'] = profile
                result = subprocess.run(['bash', str(script), *args], cwd=cwd, env=env)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(json.loads(run_log.read_text()), [str(cwd), args])
                build = json.loads(build_log.read_text())
                self.assertEqual(build.count('build'), 1)
                self.assertEqual(build[build.index('--target-dir') + 1], str(root / 'target'))
                self.assertEqual(build[build.index('--manifest-path') + 1], str(root / 'Cargo.toml'))
                self.assertEqual(build[build.index('--profile') + 1], profile)
                self.assertIn('holonomy-native/native-ui', build)
            run_log.unlink()
            env['BUILD_EXIT'] = '17'
            self.assertEqual(subprocess.run(['bash', str(script)], cwd=cwd, env=env).returncode, 17)
            self.assertFalse(run_log.exists(), 'failed build must not run a stale executable')
            env['BUILD_EXIT'], env['APP_EXIT'] = '0', '23'
            self.assertEqual(subprocess.run(['bash', str(script)], cwd=cwd, env=env).returncode, 23)
            env['HOLONOMY_BUILD_PROFILE'] = 'invalid'
            build_log.unlink()
            self.assertEqual(subprocess.run(['bash', str(script)], cwd=cwd, env=env).returncode, 2)
            self.assertFalse(build_log.exists())


if __name__ == '__main__':
    unittest.main()
