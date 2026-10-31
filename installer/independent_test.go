package main

import "testing"

func TestIndependentOption(t *testing.T) {
	o, err := parseArgs([]string{"--independent", "192.168.1.50"})
	if err != nil || !o.independent || len(o.targets) != 1 {
		t.Fatalf("independent option: %+v, %v", o, err)
	}
	if _, err := parseArgs([]string{"--independent", "--no-enable"}); err == nil {
		t.Fatal("independent setup must not bypass the health check")
	}
}
