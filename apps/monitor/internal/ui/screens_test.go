package ui

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/fixture"
)

// screen is one captured frame: the steps that reach it from a fresh model.
type screen struct {
	name  string
	steps []tea.Msg
}

var screens = []screen{
	{"agents", nil},
	{"agents-j", []tea.Msg{key("j")}},
	{"threads", []tea.Msg{key("2")}},
	{"services", []tea.Msg{key("3")}},
	{"jobs", []tea.Msg{key("4")}},
	{"usage", []tea.Msg{key("5")}},
	{"logs", []tea.Msg{key("6")}},
	{"background", []tea.Msg{key("7")}},
	{"config", []tea.Msg{key("8")}},
	{"transcript", []tea.Msg{special(tea.KeyEnter)}},
	{"tooldetail", []tea.Msg{special(tea.KeyEnter), key("p"), key("p"), special(tea.KeyEnter)}},
	{"trajectory", []tea.Msg{special(tea.KeyEnter), key("i")}},
	{"help", []tea.Msg{key("?")}},
}

// capture builds a model on scenario at w×h, runs steps, and returns the
// frame. Loading fetches are cut short so the frame shows the loading state.
func capture(t *testing.T, scenario fixture.Scenario, w, h int, steps []tea.Msg) string {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	clock := func() time.Time { return fixture.Base }
	b := fixture.New(scenario, clock)
	m := New(ctx, b, Options{Root: "/Users/example/Documents", Home: "/Users/example/.zdx", Now: clock})
	m.Update(tea.WindowSizeMsg{Width: w, Height: h})
	drive := func(cmd tea.Cmd) {
		if scenario == fixture.Loading {
			return
		}
		pump(m, cmd)
	}
	drive(m.refresh(clock()))
	if scenario == fixture.Stale {
		// A second round fails: everything shown is now stale.
		now := fixture.Base.Add(10 * time.Minute)
		m.opts.Now = func() time.Time { return now }
		for k := range m.fetch {
			m.fetch[k].force = true
		}
		drive(m.refresh(now))
	}
	for _, s := range steps {
		_, cmd := m.Update(s)
		drive(cmd)
	}
	return m.render()
}

// TestDumpScreens writes every screen of every scenario at three sizes to
// ZDX_MONITOR_TEST_DUMP_DIR (`.ans` with styles, `.txt` plain) for review.
func TestDumpScreens(t *testing.T) {
	dir := os.Getenv("ZDX_MONITOR_TEST_DUMP_DIR")
	_ = os.MkdirAll(dir, 0o755)
	if dir == "" {
		t.Skip("set ZDX_MONITOR_TEST_DUMP_DIR to write screen captures")
	}
	sizes := [][2]int{{60, 16}, {80, 24}, {120, 34}, {180, 48}}
	for _, sc := range fixture.Scenarios {
		for _, size := range sizes {
			for _, s := range screens {
				if sc != fixture.Full && s.name != "agents" && s.name != "threads" && s.name != "services" && s.name != "transcript" {
					continue
				}
				frame := capture(t, sc, size[0], size[1], s.steps)
				base := filepath.Join(dir, fmt.Sprintf("%s-%dx%d-%s", sc, size[0], size[1], s.name))
				if err := os.WriteFile(base+".ans", []byte(frame), 0o644); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(base+".txt", []byte(stripANSI(frame)), 0o644); err != nil {
					t.Fatal(err)
				}
			}
		}
	}
}

func stripANSI(s string) string { return ansi.Strip(s) }

var frameSizes = [][2]int{{60, 16}, {80, 24}, {120, 34}, {200, 50}}

// TestFramesFitTheTerminal renders every screen of every scenario at
// small and large sizes: each frame fills the height exactly and no line
// is wider than the terminal.
func TestFramesFitTheTerminal(t *testing.T) {
	for _, sc := range fixture.Scenarios {
		for _, size := range frameSizes {
			for _, s := range screens {
				frame := capture(t, sc, size[0], size[1], s.steps)
				lines := strings.Split(frame, "\n")
				if len(lines) != size[1] {
					t.Errorf("%s %dx%d %s: %d lines, want %d", sc, size[0], size[1], s.name, len(lines), size[1])
				}
				for i, l := range lines {
					if w := ansi.StringWidth(l); w > size[0] {
						t.Errorf("%s %dx%d %s line %d is %d wide: %q", sc, size[0], size[1], s.name, i, w, ansi.Strip(l))
					}
				}
			}
		}
	}
}

// TestTinyTerminalsNeverOverflow: below the layout's size the frame still
// stays inside the terminal (no wrapped or scrolled lines).
func TestTinyTerminalsNeverOverflow(t *testing.T) {
	for _, size := range [][2]int{{1, 1}, {10, 4}, {19, 30}, {30, 6}, {60, 8}, {200, 7}} {
		for _, s := range screens {
			frame := capture(t, fixture.Full, size[0], size[1], s.steps)
			lines := strings.Split(frame, "\n")
			if len(lines) > size[1] {
				t.Errorf("%dx%d %s: %d lines", size[0], size[1], s.name, len(lines))
			}
			for i, l := range lines {
				if w := ansi.StringWidth(l); w > size[0] {
					t.Errorf("%dx%d %s line %d is %d wide", size[0], size[1], s.name, i, w)
				}
			}
		}
	}
}

// golden are the frames kept as text snapshots in testdata/screens.
var golden = []struct {
	scenario fixture.Scenario
	w, h     int
	screen   string
}{
	{fixture.Full, 80, 24, "agents"},
	{fixture.Full, 120, 34, "agents"},
	{fixture.Full, 80, 24, "threads"},
	{fixture.Full, 120, 34, "services"},
	{fixture.Full, 120, 34, "jobs"},
	{fixture.Full, 80, 24, "usage"},
	{fixture.Full, 120, 34, "usage"},
	{fixture.Full, 120, 34, "logs"},
	{fixture.Full, 80, 24, "transcript"},
	{fixture.Full, 120, 34, "tooldetail"},
	{fixture.Full, 80, 24, "trajectory"},
	{fixture.Full, 120, 34, "config"},
	{fixture.Full, 60, 16, "agents"},
	{fixture.Down, 80, 24, "services"},
	{fixture.Down, 80, 24, "agents"},
	{fixture.Empty, 80, 24, "threads"},
	{fixture.Loading, 80, 24, "agents"},
	{fixture.Stale, 80, 24, "agents"},
}

// TestScreenSnapshots compares frames with testdata/screens. Regenerate
// after an intended change with ZDX_MONITOR_TEST_UPDATE=1, then review the
// diff.
func TestScreenSnapshots(t *testing.T) {
	update := os.Getenv("ZDX_MONITOR_TEST_UPDATE") != ""
	steps := map[string][]tea.Msg{}
	for _, s := range screens {
		steps[s.name] = s.steps
	}
	for _, g := range golden {
		name := fmt.Sprintf("%s-%dx%d-%s.txt", g.scenario, g.w, g.h, g.screen)
		got := trimRight(stripANSI(capture(t, g.scenario, g.w, g.h, steps[g.screen])))
		path := filepath.Join("testdata", "screens", name)
		if update {
			if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(path, []byte(got), 0o644); err != nil {
				t.Fatal(err)
			}
			continue
		}
		want, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("%s: %v (run with ZDX_MONITOR_TEST_UPDATE=1 to create it)", name, err)
		}
		if got != string(want) {
			t.Errorf("%s differs from the snapshot:\n--- got\n%s\n--- want\n%s", name, got, want)
		}
	}
}

// trimRight drops trailing spaces so snapshots diff cleanly.
func trimRight(s string) string {
	lines := strings.Split(s, "\n")
	for i, l := range lines {
		lines[i] = strings.TrimRight(l, " ")
	}
	return strings.Join(lines, "\n") + "\n"
}

// TestResizeKeepsTheSelection selects a row, shrinks and grows the
// terminal, and refreshes: the same row stays selected and visible.
func TestResizeKeepsTheSelection(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	clock := func() time.Time { return fixture.Base }
	m := New(ctx, fixture.New(fixture.Full, clock), Options{Root: "/r", Home: "/h", Now: clock})
	m.Update(tea.WindowSizeMsg{Width: 120, Height: 34})
	pump(m, m.refresh(clock()))
	_, cmd := m.Update(key("2"))
	pump(m, cmd)
	for i := 0; i < 5; i++ {
		m.Update(key("j"))
	}
	want := m.threads.selectedThread().ID
	for _, size := range [][2]int{{60, 12}, {200, 50}, {80, 24}} {
		m.Update(tea.WindowSizeMsg{Width: size[0], Height: size[1]})
		m.fetch[keyThreads].force = true
		pump(m, m.refresh(clock()))
		frame := stripANSI(m.render())
		if got := m.threads.selectedThread().ID; got != want {
			t.Fatalf("%v: selection moved to %s", size, got)
		}
		if !strings.Contains(frame, "▌") {
			t.Fatalf("%v: the selected row is not visible:\n%s", size, frame)
		}
	}
}

func TestTableKeepsWideCharactersAligned(t *testing.T) {
	tb := table{cols: []col{{title: "A", max: 6}, {title: "B", flex: true, min: 4}}}
	tb.rows = [][]cell{{c("日本語テキスト"), c("ok")}, {c("ab"), c("🚀 launch the rocket")}}
	tb.layout(20)
	for i := range tb.rows {
		for _, sel := range []bool{false, true} {
			line := tb.row(i, sel, 20)
			if w := ansi.StringWidth(line); w > 20 || (sel && w != 20) {
				t.Fatalf("row %d selected=%v is %d wide: %q", i, sel, w, ansi.Strip(line))
			}
		}
	}
	// The second column starts at the same cell on every row.
	h := ansi.Strip(tb.header())
	r := ansi.Strip(tb.row(0, false, 20))
	if strings.Index(h, "B") != ansi.StringWidth(r[:strings.Index(r, "ok")]) {
		t.Fatalf("misaligned:\n%q\n%q", h, r)
	}
}

// TestAgentsListShowsTheModelSpecOnly: the list shows each run's model as
// one `provider[@account]:model@thinking` spec and no activity text; the
// activity and every spec part are in the selected run's detail.
func TestAgentsListShowsTheModelSpecOnly(t *testing.T) {
	for _, w := range []int{60, 80, 120, 200} {
		frame := stripANSI(capture(t, fixture.Full, w, 34, nil))
		list, detail, found := strings.Cut(frame, "Selected run")
		if !found {
			t.Fatalf("%d: no detail pane:\n%s", w, frame)
		}
		for _, banned := range []string{"ACTIVITY", "PROVIDER", "THINKING", "retrying after 529", "waiting for model", "bash: go test"} {
			if strings.Contains(list, banned) {
				t.Fatalf("%d: the list shows %q:\n%s", w, banned, list)
			}
		}
		wants := []string{"MODEL", "openai-codex@work:gpt-6-astra@high", "claude-cli@work:claude-opus-5-5@high"}
		if w < 80 {
			// Narrow: the account goes first, then the middle of the spec.
			wants = []string{"MODEL", "openai-cod…:gpt-6-astra@high", "claude…:claude-opus-5-5@high", "…hinking-exp-2026-11@minimal"}
		}
		for _, want := range wants {
			if !strings.Contains(list, want) {
				t.Fatalf("%d: list lacks %q:\n%s", w, want, list)
			}
		}
		if strings.Contains(list, "claude-cli:claude-cli") || strings.Contains(list, "@high@") {
			t.Fatalf("%d: a spec part is repeated:\n%s", w, list)
		}
		for _, want := range []string{"Activity   tool  bash: go test -race ./... (+1 more)", "Model      openai-codex@work:gpt-6-astra@high"} {
			if !strings.Contains(detail, want) {
				t.Fatalf("%d: detail lacks %q:\n%s", w, want, detail)
			}
		}
	}
}

// cutDuration matches a duration cut by a column: "1h 2…", "17…".
var cutDuration = regexp.MustCompile(`\b[0-9]+[smhd]? ?[0-9]*…`)

// TestDurationColumnsAreNeverCut: list columns show one unit of each
// duration ("1h", "12d", "100h"), so no cell ends in "…"; the selected
// thread's detail keeps the full age.
func TestDurationColumnsAreNeverCut(t *testing.T) {
	cases := []struct {
		screen string
		want   []string
	}{
		{"threads", []string{" 37s ", " 12m ", " 1h ", " 17h ", " 12d ", " 365d "}},
		{"agents", []string{" 13m", " 100h", " 2m", " 9s"}},
		{"background", []string{" 42m ", " 6m ", " 1h "}},
		{"jobs", []string{"in 17h", "✓ 6h ago", "✗ failed 19h ago", "in 1d"}},
	}
	steps := map[string][]tea.Msg{}
	for _, s := range screens {
		steps[s.name] = s.steps
	}
	for _, w := range []int{60, 80, 120, 200} {
		for _, tc := range cases {
			frame := stripANSI(capture(t, fixture.Full, w, 40, steps[tc.screen]))
			list, _, _ := strings.Cut(frame, "── Selected")
			for _, l := range strings.Split(list, "\n") {
				if cutDuration.MatchString(l) {
					t.Errorf("%d %s: a duration is cut: %q", w, tc.screen, l)
				}
			}
			for _, want := range tc.want {
				// Narrow tables hide some columns; check what is shown.
				if tc.screen == "agents" && w < 80 || tc.screen == "jobs" && w < 70 {
					continue
				}
				if !strings.Contains(list, want) {
					t.Errorf("%d %s: list lacks %q:\n%s", w, tc.screen, want, list)
				}
			}
		}
	}
	frame := stripANSI(capture(t, fixture.Full, 80, 30, []tea.Msg{key("2"), key("j"), key("j"), key("j")}))
	if !strings.Contains(frame, "Last write 17h 58m ago") {
		t.Fatalf("the detail must keep the full age:\n%s", frame)
	}
}
