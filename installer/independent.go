package main

import (
	"fmt"
	"os"
	"path/filepath"
)

// installIndependent installs the optional Mini 2 configuration only after pulsar is healthy.
func installIndependent(s string) bool {
	mounts, err := sh(s, "cat /proc/mounts")
	if err != nil {
		fmt.Fprintln(os.Stderr, "cannot read mounts")
		return false
	}
	if _, found := rootMountRW(mounts); !found {
		fmt.Fprintln(os.Stderr, "root mount not found")
		return false
	}
	cmdline, err := sh(s, "cat /proc/cmdline")
	if err != nil {
		fmt.Fprintln(os.Stderr, "cannot read rootfs configuration")
		return false
	}
	mapper, _ := sh(s, "ls /dev/mapper 2>/dev/null | cat")
	df, err := sh(s, "df /etc /system/vendor/quasar")
	if err != nil {
		fmt.Fprintln(os.Stderr, "cannot check rootfs")
		return false
	}
	if reason := rootfsGuard(cmdline, mapper, df); reason != "" {
		fmt.Fprintf(os.Stderr, "--independent refused: %s\n", reason)
		return false
	}
	local := filepath.Join(root, "satellite", "deploy", "independent.sh")
	remote := dataDir + "/independent-install.tmp"
	if push(s, local, remote) != nil {
		return false
	}
	defer sh(s, "rm -f "+remote)
	if _, err := sh(s, "sh -n "+remote); err != nil {
		return false
	}
	if err := shTo(os.Stdout, s, "sh "+remote+" install"); err != nil {
		fmt.Fprintln(os.Stderr, "independent setup failed; inspect output and backups before retrying")
		return false
	}
	return true
}
