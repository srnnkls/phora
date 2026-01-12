package main

import (
	"os"

	"github.com/srnnkls/tropos/internal/cli"
)

func main() {
	if err := cli.Execute(); err != nil {
		os.Exit(1)
	}
}
