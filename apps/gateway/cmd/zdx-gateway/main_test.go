package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestServeRefusesAndCheckIsReadOnly(t *testing.T) {
	// Short path: macOS limits Unix socket paths to 104 bytes.
	dir, err := os.MkdirTemp("", "gwc")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(dir)
	settings := filepath.Join(dir, "s.json")
	os.WriteFile(settings, []byte(`{"bot_token":"1:SECRET","allowlist_user_ids":[7],"root":"/r"}`), 0o600)
	socket := filepath.Join(dir, "none.sock")

	err = run([]string{"serve", "--settings", settings, "--socket", socket}, nil, &bytes.Buffer{})
	if err == nil || !strings.Contains(err.Error(), "serve is not available") {
		t.Fatalf("serve must refuse, got %v", err)
	}

	var out bytes.Buffer
	if err := run([]string{"check", "--settings", "-", "--socket", socket}, strings.NewReader(`{"bot_token":"1:SECRET","allowlist_user_ids":[7],"root":"/r"}`), &out); err != nil {
		t.Fatal(err)
	}
	text := out.String()
	if strings.Contains(text, "SECRET") || !strings.Contains(text, "not running") || !strings.Contains(text, "missing:") {
		t.Fatalf("unexpected check output:\n%s", text)
	}
}

func TestServiceArgvNeedsRootAndBot(t *testing.T) {
	// Never the real home: `bot` starts the gateway there.
	home, err := os.MkdirTemp("", "gwa")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	t.Setenv("ZDX_HOME", home)
	for _, args := range [][]string{{"--root"}, {"--root", "/r"}, {"--root", "/r", "serve"}} {
		if err := run(args, nil, &bytes.Buffer{}); err == nil {
			t.Errorf("%v must be refused", args)
		}
	}
	// A zdx that cannot print settings stops the start.
	err = run([]string{"--root", "/r", "bot", "--zdx", "/nonexistent/zdx"}, nil, &bytes.Buffer{})
	if err == nil || !strings.Contains(err.Error(), "telegram settings failed") {
		t.Fatalf("unexpected %v", err)
	}
}

func TestPreflightPrintsJSONAndChangesNothing(t *testing.T) {
	dir, err := os.MkdirTemp("", "gwpf")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(dir)
	zdx := filepath.Join(dir, "zdx")
	os.WriteFile(zdx, []byte("#!/bin/sh\necho '{\"bot_token\":\"1:SECRET\",\"allowlist_user_ids\":[7],\"allowlist_chat_ids\":[],\"server\":{\"enabled\":false,\"port\":4141},\"root\":\"/r\",\"profiles\":[]}'\n"), 0o755)
	t.Setenv("ZDX_HOME", dir)
	for _, args := range [][]string{
		{"preflight", "--root", dir, "--json", "--zdx", zdx},
		{"--root", dir, "preflight", "--json", "--zdx", zdx},
	} {
		var out bytes.Buffer
		if err := run(args, nil, &out); err != nil {
			t.Fatalf("%v: %v", args, err)
		}
		var r struct {
			Service  string   `json:"service"`
			Ready    *bool    `json:"ready"`
			Problems []string `json:"problems"`
		}
		// Nothing is missing and the settings load; an unreachable service
		// is only a warning.
		if err := json.Unmarshal(out.Bytes(), &r); err != nil || r.Service != "bot" || r.Ready == nil || !*r.Ready || len(r.Problems) != 0 || strings.Contains(out.String(), "SECRET") {
			t.Fatalf("%v: %s %v", args, out.String(), err)
		}
	}
	var out bytes.Buffer
	if err := run([]string{"preflight", "--root", dir, "--json", "--zdx", filepath.Join(dir, "nonexistent")}, nil, &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), `"ready":false`) || !strings.Contains(out.String(), "problems") {
		t.Fatalf("settings that do not load are a problem: %s", out.String())
	}
	if err := run([]string{"preflight", "--root", dir}, nil, &bytes.Buffer{}); err == nil {
		t.Fatal("--json is required")
	}
	entries, _ := os.ReadDir(dir)
	if len(entries) != 1 {
		t.Fatalf("preflight writes nothing: %v", entries)
	}
}
