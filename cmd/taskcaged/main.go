//go:build linux

package main

import (
	"flag"
	"log/slog"
	"os"
)

var version = "dev"

func main() {
	var socketPath string
	var cgroupRoot string
	var launcherPath string

	flag.StringVar(&socketPath, "socket", "/run/taskcage/taskcaged.sock", "Unix Domain Socket path")
	flag.StringVar(&cgroupRoot, "cgroup-root", "auto", "delegated cgroup v2 root or auto")
	flag.StringVar(&launcherPath, "launcher", "/usr/libexec/taskcage/taskcage-launcher", "Rust launcher path")
	flag.Parse()

	logger := slog.New(slog.NewJSONHandler(os.Stderr, nil))
	logger.Info(
		"TaskCage daemon scaffold",
		"version", version,
		"socket", socketPath,
		"cgroupRoot", cgroupRoot,
		"launcher", launcherPath,
	)

	logger.Error("daemon runtime is not implemented yet")
	os.Exit(2)
}
