package ui

import (
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/fixture"
	"github.com/tallesborges/zdx/apps/shared/quota"
)

func TestSplitCellsFillsTheBarAndKeepsSmallParts(t *testing.T) {
	for _, tc := range []struct {
		parts []uint64
		n     int
		want  []int
	}{
		{[]uint64{90, 9, 1}, 10, []int{8, 1, 1}},
		{[]uint64{1, 1, 1}, 2, []int{1, 1, 0}},
		{[]uint64{0, 5}, 4, []int{0, 4}},
		{[]uint64{0, 0}, 4, []int{0, 0}},
	} {
		got := splitCells(tc.parts, tc.n)
		for i := range got {
			if got[i] != tc.want[i] {
				t.Errorf("splitCells(%v, %d) = %v, want %v", tc.parts, tc.n, got, tc.want)
				break
			}
		}
	}
}

func TestDailyChartMetricAndSplitKeys(t *testing.T) {
	window := data.QuotaWindow{Label: "5h", UsedPercent: 10}
	m, frame := quotaFrame(t, fixture.Full, 120, quotaAnswer{subs: []data.Subscription{{Provider: "openai-codex", Name: "Codex", Windows: []data.QuotaWindow{window}}}})
	got := frame()
	if !strings.Contains(got, "Daily tokens ▼ 72% vs 318.4M the 7 days before") || !strings.Contains(got, "■ Claude 59.1M") {
		t.Fatalf("daily chart header and legend:\n%s", got)
	}
	if !strings.Contains(got, "Wed 11 ▕") || !strings.Contains(got, "▏ · ") {
		t.Fatalf("an idle day keeps its row:\n%s", got)
	}
	m.Update(key("m"))
	if got := frame(); !strings.Contains(got, "Daily tokens without cache reads") {
		t.Fatalf("m shows tokens without cache reads:\n%s", got)
	}
	m.Update(key("m"))
	if got := frame(); !strings.Contains(got, "Daily requests") || !strings.Contains(got, " tok ") {
		t.Fatalf("m shows requests, with tokens beside them:\n%s", got)
	}
	m.Update(key("p"))
	if got := frame(); strings.Contains(got, "■ Claude") {
		t.Fatalf("p hides the provider split:\n%s", got)
	}
}

func TestQuotaStrip(t *testing.T) {
	share := &data.QuotaHistory{Pace: 10, Days: []quota.DayUse{
		{}, {Known: true}, {Known: true, Percent: 40, Reset: true}, {Known: true, Percent: 5},
	}}
	// Scale is the largest day (40): 40 is full, 5 is one eighth.
	if got := stripANSI(quotaStrip(share)); got != "  ·│█ ▁" {
		t.Errorf("share strip = %q", got)
	}
	peak := &data.QuotaHistory{Peak: true, Days: []quota.DayUse{{Known: true, Percent: 100}, {Known: true, Percent: 50, Reset: true}}}
	// A peak window shows no reset mark: a day holds several windows.
	if got := stripANSI(quotaStrip(peak)); got != "█ ▄" {
		t.Errorf("peak strip = %q", got)
	}
	if quotaStrip(nil) != "" {
		t.Error("no history is no strip")
	}
}
