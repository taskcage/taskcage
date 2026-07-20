//go:build !linux

package main

import (
	"fmt"
	"os"
)

func main() {
	fmt.Fprintln(os.Stderr, "taskcaged requires Linux with cgroup v2")
	os.Exit(2)
}
