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
        self.write("data/chrony_is_enabled", "")
        self.write("etc/chrony.conf", "pool firmware.example iburst\n")
        self.write("system/vendor/quasar/run.sh", "#!/bin/sh\necho stock\n", 0o755)
        self.write("etc/init.d/S70unbound", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("etc/init.d/S49chrony", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("bin/unbound-checkconf", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("bin/mount", "#!/bin/sh\nexit 0\n", 0o755)
        self.write("bin/pidof", "#!/bin/sh\necho 123\n", 0o755)
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
        self.assertIn("pool pool.ntp.org iburst", (self.root / "data/chrony.conf").read_text())
        self.assertIn("CHRONY_ARGS=", (self.root / "etc/default/chrony").read_text())
        self.assertTrue((self.root / "data/pulsar/independent.sh").is_file())
        result = self.run_setup("restore")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())
        self.assertEqual((self.root / "data/unbound.conf.head").read_text(), "original dns\n")
        self.assertFalse((self.root / "etc/default/chrony").exists())

    def test_refuses_existing_custom_chrony_configuration(self):
        self.write("data/chrony.conf", "pool custom.example iburst\n")
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / "data/chrony.conf").read_text(), "pool custom.example iburst\n")
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())

    def test_chrony_restart_failure_removes_new_hook(self):
        self.write("etc/init.d/S49chrony", "#!/bin/sh\n[ \"$1\" = start ]\n", 0o755)
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "etc/default/chrony").exists())
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())

    def test_chrony_missing_after_restart_removes_new_hook(self):
        self.write("bin/pidof", "#!/bin/sh\nexit 1\n", 0o755)
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "etc/default/chrony").exists())
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())

    def test_restore_script_copy_failure_is_reported(self):
        self.script = self.root / "data/pulsar/independent.sh.new"
        source = Path("satellite/deploy/independent.sh").read_text()
        for path in ("/data", "/system", "/etc", "/proc/mounts"):
            source = source.replace(path, str(self.root) + path)
        self.script.write_text(source)
        result = self.run_setup("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot install restore script", result.stderr)
        self.assertFalse((self.root / "data/quasar/disable_stock_launcher").exists())

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
