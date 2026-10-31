#!/usr/bin/env python3
"""Check that a supervisor's replacement process is not mistaken for the old daemon."""

from pathlib import Path
import subprocess
import unittest


class RestartTest(unittest.TestCase):
    def test_stop_waits_for_original_pid(self):
        source = Path("satellite/deploy/ctl.sh").read_text()
        stop = source[source.index("stop_daemon() {"):source.index("# Starts supervise.sh")]
        script = r'''
checks=0
pidof() { echo 100; }
sleep() { :; }
kill() {
  if [ "$1" = -0 ]; then
    checks=$((checks + 1))
    [ "$2" = 100 ] && [ "$checks" -le 1 ]
  else
    [ "$1" = 100 ]
  fi
}
''' + stop + r'''
stop_daemon || exit 1
[ "$checks" -ge 2 ] || exit 1
'''
        result = subprocess.run(["sh", "-c", script], text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
