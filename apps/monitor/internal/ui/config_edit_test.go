package ui

import (
	"context"
	"strings"
	"sync"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

// configFake is a backend that can write config: it records every change
// and answers the dry run and the write.
type configFake struct {
	*fake
	mu      sync.Mutex
	changes []source.SettingChange
}

func (f *configFake) Workspaces(context.Context) (data.Workspaces, source.Origin, error) {
	global := data.SettingSource{Label: "global", Layer: "global"}
	modes := data.Setting{Group: "model modes", Key: "model_modes", Label: "modes", Kind: "modes", Value: []byte(`"fast → p:a@low"`), Source: global,
		Modes: []data.ModeView{{Name: "fast", Description: "quick", Primary: "p:a@low", Thinking: "low", Alternatives: []string{"p:b"}}, {Name: "deep", Primary: "p:a@max", Thinking: "max"}}}
	settings := []data.Setting{
		{Group: "core", Key: "model", Label: "model", Kind: "chat", Value: []byte(`"p:a@high"`), Source: global},
		modes,
		{Group: "subagents", Key: "subagents.enabled", Label: "enabled", Kind: "toggle", Value: []byte(`"on"`), Source: global},
	}
	return data.Workspaces{
		Global:     &data.GlobalConfig{Path: "/h/config.toml", Settings: settings},
		Workspaces: []data.Workspace{{Name: "proj", Root: "/r", Current: true, OverlayPath: "/r/.zdx/config.toml", Settings: settings}},
	}, source.OriginSocket, nil
}

func (f *configFake) Picker(context.Context, string) (data.Picker, error) {
	return data.Picker{
		Models: []data.PickerModel{
			{ID: "p:a", Name: "p:a", Specs: map[string]string{"off": "p:a@off", "high": "p:a@high"}},
			{ID: "p:fixed", Name: "p:fixed", Specs: map[string]string{}},
		},
		Modes: []data.PickerMode{{Name: "fast", Ref: "mode:fast"}},
	}, nil
}

func (f *configFake) ChangeSetting(_ context.Context, ch source.SettingChange) (data.ChangeOutcome, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.changes = append(f.changes, ch)
	out := data.ChangeOutcome{Key: ch.Key, File: "/r/.zdx/config.toml", DryRun: ch.DryRun, Changed: true}
	out.Reach.Reads = []string{"proj"}
	return out, nil
}

func (f *configFake) written() []source.SettingChange {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []source.SettingChange
	for _, ch := range f.changes {
		if !ch.DryRun {
			out = append(out, ch)
		}
	}
	return out
}

func configModel(t *testing.T) (*Model, *configFake) {
	t.Helper()
	f := &configFake{fake: &fake{}}
	m, _ := newModel(t, f.fake)
	m.backend = f
	pump(m, m.switchTab(tabConfig))
	return m, f
}

func press(m *Model, msg tea.KeyPressMsg) {
	_, cmd := m.Update(msg)
	pump(m, cmd)
}

func selectRow(t *testing.T, m *Model, id string) {
	t.Helper()
	for i, rid := range m.config.ids() {
		if rid == id {
			m.config.list.selected = i
			return
		}
	}
	t.Fatalf("no row %q in %v", id, m.config.ids())
}

// Each mode is its own row, and the current workspace is shown first.
func TestConfigShowsEveryModeAndTheWriteTarget(t *testing.T) {
	m, _ := configModel(t)
	got := plain(m)
	for _, want := range []string{"Scope 2 of 2", "writes /r/.zdx/config.toml", "fast", "deep", "p:a@max", "quick · or p:b"} {
		if !strings.Contains(got, want) {
			t.Fatalf("missing %q:\n%s", want, got)
		}
	}
	press(m, key("["))
	if got := plain(m); !strings.Contains(got, "every workspace inherits it") || !strings.Contains(got, "writes /h/config.toml") {
		t.Fatalf("global scope:\n%s", got)
	}
}

// A model pick goes through the level, a dry run, then the write, to the
// scope's own file.
func TestConfigEditPicksAModelThenWritesAfterTheCheck(t *testing.T) {
	m, f := configModel(t)
	selectRow(t, m, "model")
	press(m, special(tea.KeyEnter))
	for _, r := range "p:a" {
		press(m, key(string(r)))
	}
	press(m, special(tea.KeyEnter)) // p:a has levels
	if e := m.config.edit; e == nil || e.stage != stageLevel {
		t.Fatalf("expected the level step: %+v", e)
	}
	press(m, special(tea.KeyDown)) // off, high
	press(m, special(tea.KeyEnter))
	if got := plain(m); !strings.Contains(got, "Applies to") || !strings.Contains(got, "enter write") {
		t.Fatalf("confirm:\n%s", got)
	}
	if len(f.written()) != 0 {
		t.Fatal("wrote before the confirm")
	}
	press(m, special(tea.KeyEnter))
	w := f.written()
	if len(w) != 1 || w[0].Key != "model" || *w[0].Value != "p:a@high" || w[0].Workspace != "/r" {
		t.Fatalf("writes %+v", w)
	}
	if m.config.edit != nil || !strings.Contains(plain(m), "wrote model in /r/.zdx/config.toml") {
		t.Fatalf("after the write:\n%s", plain(m))
	}
}

// A model without levels is picked directly; a mode row writes the whole
// list with only its primary changed; esc cancels; x resets.
func TestConfigEditModeToggleAndReset(t *testing.T) {
	m, f := configModel(t)
	selectRow(t, m, "model_modes#deep")
	press(m, special(tea.KeyEnter))
	if e := m.config.edit; e == nil || len(e.entries()) != 2 {
		t.Fatalf("a mode primary offers models only: %+v", e.entries())
	}
	press(m, special(tea.KeyDown))
	press(m, special(tea.KeyEnter)) // p:fixed, no levels
	press(m, special(tea.KeyEnter))
	w := f.written()
	if len(w) != 1 || w[0].Key != "model_modes" {
		t.Fatalf("writes %+v", w)
	}
	want := `[{"name":"fast","description":"quick","primary":"p:a@low","alternatives":["p:b"]},{"name":"deep","description":"","primary":"p:fixed","alternatives":[]}]`
	if *w[0].Value != want {
		t.Fatalf("modes\n got %s\nwant %s", *w[0].Value, want)
	}

	selectRow(t, m, "subagents.enabled")
	press(m, special(tea.KeyEnter))
	press(m, special(tea.KeyEscape))
	if m.config.edit != nil || len(f.written()) != 1 {
		t.Fatal("esc must cancel without a write")
	}
	press(m, special(tea.KeyEnter))
	press(m, special(tea.KeyEnter))
	if w := f.written(); len(w) != 2 || *w[1].Value != "off" {
		t.Fatalf("toggle %+v", w)
	}

	selectRow(t, m, "model")
	press(m, key("x"))
	press(m, special(tea.KeyEnter))
	if w := f.written(); len(w) != 3 || w[2].Value != nil {
		t.Fatalf("reset %+v", w)
	}
}
