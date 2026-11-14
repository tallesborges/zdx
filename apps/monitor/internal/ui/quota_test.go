package ui

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/fixture"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/shared/version"
)

// quotaBackend is a fixture whose quota answers come from a queue (the last
// one repeats).
type quotaBackend struct {
	*fixture.Backend
	mu      sync.Mutex
	answers []quotaAnswer
}

type quotaAnswer struct {
	subs []data.Subscription
	err  error
}

func (b *quotaBackend) Quotas(context.Context) ([]data.Subscription, source.Origin, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	a := b.answers[0]
	if len(b.answers) > 1 {
		b.answers = b.answers[1:]
	}
	return a.subs, source.OriginDirect, a.err
}

func quotaFrame(t *testing.T, scenario fixture.Scenario, width int, answers ...quotaAnswer) (*Model, func() string) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	now := fixture.Base
	clock := func() time.Time { return now }
	m := New(ctx, &quotaBackend{Backend: fixture.New(scenario, clock), answers: answers}, Options{Root: "/r", Home: "/h", Now: clock})
	m.Update(tea.WindowSizeMsg{Width: width, Height: 40})
	pump(m, m.refresh(clock()))
	_, cmd := m.Update(key("5"))
	pump(m, cmd)
	return m, func() string { return strings.Join(strings.Fields(stripANSI(m.render())), " ") }
}

func TestUnknownVersionShowsTheWholeFixWithoutTheService(t *testing.T) {
	_, frame := quotaFrame(t, fixture.Down, 80, quotaAnswer{err: version.ErrUnknown})
	got := frame()
	if !strings.Contains(got, "Could not fetch quotas: zdx version unknown") || !strings.Contains(got, "pass --zdx-version") {
		t.Fatalf("the quota section must show the whole fix:\n%s", got)
	}
	if strings.Contains(got, "No quotas reported") {
		t.Fatalf("an error is not an empty list:\n%s", got)
	}
}

func TestFailedQuotaRefreshKeepsStaleRows(t *testing.T) {
	window := data.QuotaWindow{Label: "5h", UsedPercent: 42.5}
	ok := quotaAnswer{subs: []data.Subscription{{Provider: "openai-codex", Name: "Codex", Windows: []data.QuotaWindow{window}}}}
	m, frame := quotaFrame(t, fixture.Full, 100, ok, quotaAnswer{err: context.DeadlineExceeded})
	_, cmd := m.Update(key("Q"))
	pump(m, cmd)
	got := frame()
	if !strings.Contains(got, "Last refresh failed") || !strings.Contains(got, "Codex") {
		t.Fatalf("a failed refresh keeps the last rows and says so:\n%s", got)
	}
}
