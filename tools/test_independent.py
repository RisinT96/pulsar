#!/usr/bin/env python3
"""Exercise the optional device setup against a temporary fake filesystem."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class IndependentSetupTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for directory in ("data/pulsar", "data/quasar", "system/vendor/quasar",
                          "etc/unbound", "etc/init.d", "proc", "bin"):
            (self.root / directory).mkdir(parents=True)
        self.write("data/pulsar/enabled", "")
        self.write("data/pulsar/pulsar", "#!/bin/sh\n", 0o755)
        self.write("data/unbound.conf.head", "original dns\n")
        self.write("system/vendor/quasar/run.sh", "#!/bin/sh\necho stock\n", 0o755)
        self.write("etc/init.d/S70unbound", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("bin/unbound-checkconf", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("bin/mount", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("proc/mounts", "root / ext4 ro 0 0\n")
        source = Path("satellite/deploy/independent.sh").read_text()
        for path in ("/data", "/system", "/etc", "/proc/mounts"):
            source = source.replace(path, str(self.root) + path)
        self.script = self.root / "setup.sh"
        self.script.write_text(source)
        self.env = dict(os.environ, PATH=str(self.root / "bin") + ":" + os.environ["PATH"])

    def write(self, path, contents, mode=0o644):
        target = self.root / path
        target.write_text(contents)
        target.chmod(mode)

    def run_setup(self, action):
        return subprocess.run(["sh", str(self.script), action], env=self.env,
                              text=True, capture_output=True)

    def test_install_repeat_and_restore(self):
        for _ in range(2):
            result = self.run_setup("install")
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "data/quasar/run.sh.before-disable-stock").read_text(),
                         "#!/bin/sh\necho stock\n")
        self.assertTrue((self.root / "data/quasar/disable_stock_launcher").exists())
        self.assertIn("1.1.1.1", (self.root / "data/unbound.conf.head").read_text())
        result = self.run_setup("restore")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())
        self.assertEqual((self.root / "data/unbound.conf.head").read_text(), "original dns\n")

    def test_refuses_guard_without_original_backup(self):
        self.write("system/vendor/quasar/run.sh",
                   '#!/bin/sh\n[ ! -f ' + str(self.root) +
                   '/data/quasar/disable_stock_launcher ] || exit 0\n', 0o755)
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())
        self.assertEqual((self.root / "data/unbound.conf.head").read_text(), "original dns\n")

    def test_remount_failure_leaves_launcher_and_dns_unchanged(self):
        self.write("bin/mount", "#!/bin/sh\nexit 1\n", 0o755)
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())
        self.assertEqual((self.root / "data/unbound.conf.head").read_text(), "original dns\n")
        self.assertEqual((self.root / "system/vendor/quasar/run.sh").read_text(),
                         "#!/bin/sh\necho stock\n")


if __name__ == "__main__":
    unittest.main()
