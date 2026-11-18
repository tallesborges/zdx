package quota

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"time"
)

// Quota history: periodic readings of every window, so a surface can show
// how much of each window was used on each recent day. The jobs scheduler is
// the only writer (SampleLoop); readers take what is there.

// HistoryKeep is how long readings are kept.
const HistoryKeep = 14 * 24 * time.Hour

// SampleEvery is how often the scheduler reads quotas for the history.
const SampleEvery = 15 * time.Minute

// HistoryPath is the history file of a ZDX home.
func HistoryPath(home string) string {
	return filepath.Join(home, "state", "quota-history.jsonl")
}

// Sample is one reading of one window (one JSON line in the history file).
type Sample struct {
	At       time.Time `json:"at"`
	Provider string    `json:"provider"`
	Account  *string   `json:"account"`
	Label    string    `json:"label"`
	// Ordinal tells apart windows with the same label in one account (in
	// report order): Claude's two weekly windows are 0 (all models) and 1
	// (Opus). Scope is not the key because some scopes are figures that
	// change on every reading ("375 of 1500").
	Ordinal     int        `json:"ordinal"`
	UsedPercent float64    `json:"used_percent"`
	ResetsAt    *time.Time `json:"resets_at"`
}

// SamplesOf is one reading of every window in a snapshot, taken at `at`.
// Failed accounts have no samples.
func SamplesOf(s Snapshot, at time.Time) []Sample {
	var out []Sample
	for _, r := range s.Providers {
		if r.Quota == nil {
			continue
		}
		seen := map[string]int{}
		for _, w := range r.Quota.Windows {
			var reset *time.Time
			if w.ResetsAt != nil {
				t := w.ResetsAt.UTC()
				reset = &t
			}
			out = append(out, Sample{At: at.UTC(), Provider: r.Provider, Account: r.Account, Label: w.Label,
				Ordinal: seen[w.Label], UsedPercent: clampPercent(w.UsedPercent), ResetsAt: reset})
			seen[w.Label]++
		}
	}
	return out
}

// AppendHistory appends samples to the history file in one write. When its
// first reading is more than a day past HistoryKeep, the file is first
// rewritten (temp file and rename) without readings older than HistoryKeep.
func AppendHistory(path string, samples []Sample, now time.Time) error {
	if len(samples) == 0 {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create %s: %w", filepath.Dir(path), err)
	}
	if err := pruneHistory(path, now); err != nil {
		return err
	}
	var buf bytes.Buffer
	for _, s := range samples {
		line, err := json.Marshal(s)
		if err != nil {
			return fmt.Errorf("encode a quota reading: %w", err)
		}
		buf.Write(line)
		buf.WriteByte('\n')
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	if _, err := f.Write(buf.Bytes()); err != nil {
		f.Close()
		return fmt.Errorf("append to %s: %w", path, err)
	}
	return f.Close()
}

func pruneHistory(path string, now time.Time) error {
	f, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	first, err := bufio.NewReader(f).ReadBytes('\n')
	f.Close()
	if err != nil && !errors.Is(err, io.EOF) {
		return fmt.Errorf("read %s: %w", path, err)
	}
	var s Sample
	if json.Unmarshal(first, &s) == nil && !s.At.Before(now.Add(-HistoryKeep-24*time.Hour)) {
		return nil
	}
	keep, err := ReadHistory(path, now.Add(-HistoryKeep))
	if err != nil {
		return err
	}
	var buf bytes.Buffer
	for _, s := range keep {
		line, err := json.Marshal(s)
		if err != nil {
			return fmt.Errorf("encode a quota reading: %w", err)
		}
		buf.Write(line)
		buf.WriteByte('\n')
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, buf.Bytes(), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", tmp, err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return fmt.Errorf("replace %s: %w", path, err)
	}
	return nil
}

// ReadHistory reads the readings at or after since, in file order. A missing
// file is no readings; a damaged line is skipped.
func ReadHistory(path string, since time.Time) ([]Sample, error) {
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	var out []Sample
	for line := range bytes.SplitSeq(data, []byte("\n")) {
		var s Sample
		if len(line) == 0 || json.Unmarshal(line, &s) != nil || s.At.Before(since) {
			continue
		}
		out = append(out, s)
	}
	return out, nil
}

// SampleLoop reads every account's quota now and then every `every`, and
// appends the windows to the history file, until ctx ends. A failed reading
// is logged and skipped (its accounts' errors are not recorded).
func SampleLoop(ctx context.Context, o Options, path string, every time.Duration, log *slog.Logger) {
	for {
		readCtx, cancel := context.WithTimeout(ctx, 2*time.Minute)
		snap, err := Fetch(readCtx, o)
		cancel()
		now := time.Now()
		switch {
		case ctx.Err() != nil:
			return
		case err != nil:
			log.Warn("Quota history reading failed", "error", err)
		default:
			if err := AppendHistory(path, SamplesOf(snap, now), now); err != nil {
				log.Warn("Quota history write failed", "error", err)
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(every):
		}
	}
}

// DayUse is one UTC day of a window's history.
type DayUse struct {
	// Known is false when the history has no reading for the day (for a
	// share window: no reading with an earlier one to compare).
	Known bool
	// Percent is the day's highest reading for a peak window, and the
	// percent of the window used during the day for a share window.
	Percent float64
	// Reset: a share window started again during the day.
	Reset bool
}

// WindowHistory is the daily use of one window over the last days.
type WindowHistory struct {
	Provider string
	Name     string // AccountDisplay, as in Subscription.Name
	Label    string
	Ordinal  int
	// Peak: the window is shorter than a day (such as 5h), so a day holds
	// several windows and Percent is the day's highest reading.
	Peak bool
	// Pace is the even daily share of a share window (100/7 for a weekly
	// window), 0 when the window length is unknown.
	Pace float64
	Days []DayUse // oldest first; the last is today (UTC)
}

// IsPeakLabel reports whether a window label names a window shorter than a
// day ("5h", "30m").
func IsPeakLabel(label string) bool {
	if len(label) < 2 {
		return false
	}
	n, err := strconv.Atoi(label[:len(label)-1])
	if err != nil || n <= 0 {
		return false
	}
	switch label[len(label)-1] {
	case 'h':
		return n < 24
	case 'm':
		return n < 24*60
	}
	return false
}

// DailyHistory is the daily use of every window in samples over the `days`
// UTC days that end on now's day. Share windows add up the growth between
// readings: a reading after a reset (the reset time moved more than an hour
// later, the earlier reset time passed, or use fell by more than 5 points)
// counts from 0 and marks the day.
func DailyHistory(samples []Sample, now time.Time, days int) []WindowHistory {
	type key struct {
		provider, account, label string
		ordinal                  int
	}
	groups := map[key][]Sample{}
	var keys []key
	for _, s := range samples {
		k := key{s.Provider, "", s.Label, s.Ordinal}
		if s.Account != nil {
			k.account = "@" + *s.Account
		}
		if _, ok := groups[k]; !ok {
			keys = append(keys, k)
		}
		groups[k] = append(groups[k], s)
	}
	today := now.UTC().Unix() / 86400
	first := today - int64(days) + 1
	index := func(t time.Time) int { return int(t.UTC().Unix()/86400 - first) }
	var out []WindowHistory
	for _, k := range keys {
		g := groups[k]
		sort.SliceStable(g, func(i, j int) bool { return g[i].At.Before(g[j].At) })
		h := WindowHistory{Provider: k.provider, Name: AccountDisplay(k.provider, g[0].Account), Label: k.label, Ordinal: k.ordinal,
			Peak: IsPeakLabel(k.label), Days: make([]DayUse, days)}
		if k.label == "weekly" {
			h.Pace = 100.0 / 7
		}
		for i, c := range g {
			d := index(c.At)
			if d < 0 || d >= days {
				continue
			}
			day := &h.Days[d]
			if h.Peak {
				day.Known = true
				day.Percent = math.Max(day.Percent, c.UsedPercent)
				continue
			}
			if i == 0 {
				continue
			}
			p := g[i-1]
			day.Known = true
			if windowReset(p, c) {
				day.Percent += c.UsedPercent
				day.Reset = true
			} else {
				day.Percent += math.Max(c.UsedPercent-p.UsedPercent, 0)
			}
		}
		out = append(out, h)
	}
	return out
}

func windowReset(p, c Sample) bool {
	if c.UsedPercent < p.UsedPercent-5 {
		return true
	}
	if p.ResetsAt == nil {
		return c.UsedPercent < p.UsedPercent-1
	}
	if !c.At.Before(*p.ResetsAt) {
		return true
	}
	return c.ResetsAt != nil && c.ResetsAt.Sub(*p.ResetsAt) > time.Hour
}

// HistoryOf finds the history of window i of a Subscription row (the same
// account, label, and ordinal), or nil.
func HistoryOf(h []WindowHistory, sub Subscription, i int) *WindowHistory {
	label := sub.Windows[i].Label
	ordinal := 0
	for _, w := range sub.Windows[:i] {
		if w.Label == label {
			ordinal++
		}
	}
	for j := range h {
		if h[j].Provider == sub.Provider && h[j].Name == sub.Name && h[j].Label == label && h[j].Ordinal == ordinal {
			return &h[j]
		}
	}
	return nil
}
