package gateway

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

// Profile edits apply to the next message without a restart, in every copy
// of the settings; the token and allowlists stay as started; a failed load
// keeps the last good profiles.
func TestSettingsFollowProfileEdits(t *testing.T) {
	dir := t.TempDir()
	layer := filepath.Join(dir, "config.toml")
	missing := filepath.Join(dir, "project", ".zdx", "config.toml")
	if err := os.WriteFile(layer, []byte("a"), 0o600); err != nil {
		t.Fatal(err)
	}
	base := Settings{BotToken: "1:x", AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/default",
		Profiles: []Profile{{Name: "p", ChatID: -100, Cwd: "/old"}}, ConfigLayers: []string{layer, missing}}
	loads := 0
	var next Settings
	var loadErr error
	s := WatchSettings(base, func(context.Context) (Settings, error) { loads++; return next, loadErr }, nil)
	copied := s
	ctx := context.Background()

	s.Refresh(ctx)
	if loads != 0 || copied.RootForChat(-100) != "/old" {
		t.Fatalf("no change, no load: loads=%d", loads)
	}

	next = base
	next.BotToken = "2:other"
	next.Profiles = []Profile{{Name: "p", ChatID: -100, Cwd: "/new", Orchestrator: true}, {Name: "q", ChatID: -200, Cwd: "/q"}}
	if err := os.WriteFile(layer, []byte("ab"), 0o600); err != nil {
		t.Fatal(err)
	}
	s.Refresh(ctx)
	if loads != 1 || copied.RootForChat(-100) != "/new" || copied.RootForChat(-200) != "/q" || copied.RootForChat(-300) != "/default" {
		t.Fatalf("an edit applies to every copy: loads=%d root=%s", loads, copied.RootForChat(-100))
	}
	chats := settingsChats{settings: copied}
	if !chats.OrchestratorChat(-100) || chats.Binding(-200).Root != "/q" {
		t.Fatal("chat bindings must follow the edit")
	}
	if copied.BotToken != "1:x" || len(copied.AllowlistChatIDs) != 1 {
		t.Fatal("token and allowlists stay as started")
	}
	s.Refresh(ctx)
	if loads != 1 {
		t.Fatal("no second load without a change")
	}

	// A new layer file counts as a change.
	os.MkdirAll(filepath.Dir(missing), 0o700)
	if err := os.WriteFile(missing, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	loadErr = errors.New("broken config")
	s.Refresh(ctx)
	s.Refresh(ctx)
	if loads != 2 || copied.RootForChat(-100) != "/new" {
		t.Fatalf("a failed load keeps the last profiles and is not retried: loads=%d", loads)
	}
	loadErr = nil
	next.Profiles = nil
	if err := os.WriteFile(missing, []byte("xy"), 0o600); err != nil {
		t.Fatal(err)
	}
	s.Refresh(ctx)
	if loads != 3 || copied.RootForChat(-100) != "/default" {
		t.Fatal("a fixed config applies; a removed profile falls back to the default root")
	}
}

// Settings without config_layers (or without a watch) never reload.
func TestSettingsWithoutLayersStayAsStarted(t *testing.T) {
	loads := 0
	s := WatchSettings(Settings{Root: "/r"}, func(context.Context) (Settings, error) { loads++; return Settings{}, nil }, nil)
	s.Refresh(context.Background())
	Settings{}.Refresh(context.Background())
	if loads != 0 {
		t.Fatal("nothing to watch")
	}
}
