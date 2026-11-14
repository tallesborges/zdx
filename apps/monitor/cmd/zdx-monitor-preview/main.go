// Command zdx-monitor-preview runs the monitor UI on synthetic fixture
// data, for checking the layout without a ZDX home, a service, or a zdx
// binary.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/fixture"
	"github.com/tallesborges/zdx/apps/monitor/internal/ui"
)

func main() {
	var names []string
	for _, s := range fixture.Scenarios {
		names = append(names, string(s))
	}
	scenario := flag.String("scenario", string(fixture.Full), "fixture scenario: "+strings.Join(names, ", "))
	flag.Parse()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	b := fixture.New(fixture.Scenario(*scenario), nil)
	m := ui.New(ctx, b, ui.Options{Root: "/Users/example/Documents", Home: "/Users/example/.zdx"})
	if _, err := tea.NewProgram(m, tea.WithContext(ctx)).Run(); err != nil {
		fmt.Fprintln(os.Stderr, "zdx-monitor-preview:", err)
		os.Exit(1)
	}
}
