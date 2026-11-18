package quota

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSamplesOfKeysWindowsByLabelOrder(t *testing.T) {
	at := time.Date(2026, 3, 4, 10, 0, 0, 0, time.UTC)
	reset := at.Add(48 * time.Hour)
	work := "work"
	snap := Snapshot{Providers: []Result{
		{Provider: ProviderClaude, Quota: &Quota{Windows: []Window{
			{Label: "5h", UsedPercent: 140},
			{Label: "weekly", UsedPercent: 20, ResetsAt: &reset},
			{Label: "weekly", UsedPercent: 5, Scope: str("Opus")},
		}}},
		{Provider: ProviderClaude, Account: &work, Err: failure(KindExpired)},
	}}
	got := SamplesOf(snap, at)
	if len(got) != 3 {
		t.Fatalf("got %d samples, want 3 (failed accounts have none)", len(got))
	}
	if got[0].UsedPercent != 100 {
		t.Errorf("used percent is not clamped: %v", got[0].UsedPercent)
	}
	if got[1].Ordinal != 0 || got[2].Ordinal != 1 || got[0].Ordinal != 0 {
		t.Errorf("ordinals = %d %d %d, want 0 0 1", got[0].Ordinal, got[1].Ordinal, got[2].Ordinal)
	}
	if got[1].ResetsAt == nil || !got[1].ResetsAt.Equal(reset) {
		t.Errorf("reset = %v, want %v", got[1].ResetsAt, reset)
	}
}

func TestAppendHistoryPrunesOldReadings(t *testing.T) {
	path := HistoryPath(t.TempDir())
	now := time.Date(2026, 3, 20, 12, 0, 0, 0, time.UTC)
	old := Sample{At: now.Add(-HistoryKeep - 25*time.Hour), Provider: ProviderCodex, Label: "weekly", UsedPercent: 1}
	recent := Sample{At: now.Add(-time.Hour), Provider: ProviderCodex, Label: "weekly", UsedPercent: 2}
	if err := AppendHistory(path, []Sample{old, recent}, old.At); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	f.WriteString("not json\n")
	f.Close()

	next := Sample{At: now, Provider: ProviderCodex, Label: "weekly", UsedPercent: 3}
	if err := AppendHistory(path, []Sample{next}, now); err != nil {
		t.Fatal(err)
	}
	got, err := ReadHistory(path, time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 || got[0].UsedPercent != 2 || got[1].UsedPercent != 3 {
		t.Fatalf("history after prune = %+v, want the recent and the new reading", got)
	}
	if _, err := os.Stat(path + ".tmp"); !os.IsNotExist(err) {
		t.Errorf("temp file left behind: %v", err)
	}
	if missing, err := ReadHistory(filepath.Join(t.TempDir(), "none.jsonl"), time.Time{}); err != nil || missing != nil {
		t.Errorf("missing file = %v, %v; want no readings", missing, err)
	}
}

func TestDailyHistory(t *testing.T) {
	now := time.Date(2026, 3, 7, 9, 0, 0, 0, time.UTC) // day 6 of the strip
	day := func(d, h int) time.Time { return time.Date(2026, 3, 1+d, h, 0, 0, 0, time.UTC) }
	reset1, reset2 := day(3, 20), day(10, 20)
	weekly := func(at time.Time, used float64, reset *time.Time) Sample {
		return Sample{At: at, Provider: ProviderClaude, Label: "weekly", UsedPercent: used, ResetsAt: reset}
	}
	five := func(at time.Time, used float64) Sample {
		return Sample{At: at, Provider: ProviderClaude, Label: "5h", UsedPercent: used}
	}
	samples := []Sample{
		weekly(day(-3, 10), 1, &reset1), // before the strip
		weekly(day(1, 10), 10, &reset1),
		weekly(day(1, 18), 25, &reset1),
		weekly(day(2, 9), 40, &reset1),
		weekly(day(3, 21), 6, &reset2), // the window started again
		weekly(day(3, 23), 4, &reset2), // a small drop is not a reset
		five(day(1, 9), 30), five(day(1, 14), 80), five(day(1, 20), 10),
		five(day(5, 9), 12),
	}
	got := DailyHistory(samples, now, 7)
	if len(got) != 2 {
		t.Fatalf("got %d windows, want 2", len(got))
	}
	w, f := got[0], got[1]
	if w.Peak || w.Pace == 0 || !f.Peak || f.Pace != 0 {
		t.Fatalf("kinds: weekly peak=%v pace=%v, 5h peak=%v pace=%v", w.Peak, w.Pace, f.Peak, f.Pace)
	}
	if w.Name != "Claude" || len(w.Days) != 7 {
		t.Fatalf("weekly = %+v", w)
	}
	want := []DayUse{{}, {Known: true, Percent: 24}, {Known: true, Percent: 15}, {Known: true, Percent: 6, Reset: true}, {}, {}, {}}
	for i, d := range w.Days {
		if d != want[i] {
			t.Errorf("weekly day %d = %+v, want %+v", i, d, want[i])
		}
	}
	if f.Days[1] != (DayUse{Known: true, Percent: 80}) || f.Days[5] != (DayUse{Known: true, Percent: 12}) || f.Days[2].Known {
		t.Errorf("5h days = %+v", f.Days)
	}

	sub := Subscription{Provider: ProviderClaude, Name: "Claude", Windows: []SubscriptionWindow{{Label: "5h"}, {Label: "weekly"}, {Label: "weekly"}}}
	if h := HistoryOf(got, sub, 1); h == nil || h.Label != "weekly" {
		t.Errorf("HistoryOf weekly = %+v", h)
	}
	if h := HistoryOf(got, sub, 2); h != nil {
		t.Errorf("HistoryOf the second weekly window = %+v, want none", h)
	}
}

func TestIsPeakLabel(t *testing.T) {
	for label, want := range map[string]bool{"5h": true, "30m": true, "24h": false, "weekly": false, "h": false, "rolling": false} {
		if IsPeakLabel(label) != want {
			t.Errorf("IsPeakLabel(%q) = %v", label, !want)
		}
	}
}
