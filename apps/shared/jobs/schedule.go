package jobs

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"
)

const (
	// DefaultOnceGraceSecs is how late a once job may still start.
	DefaultOnceGraceSecs uint32 = 6 * 60 * 60
	// DefaultRecurringGraceSecs is the grace of a recurring job's latest
	// occurrence.
	DefaultRecurringGraceSecs uint32 = 10 * 60
	// MinGraceSecs is the smallest accepted grace_secs; below one daemon poll
	// it would miss runs.
	MinGraceSecs uint32 = 60
	// MaxGraceSecs is the largest accepted grace_secs.
	MaxGraceSecs uint32 = 7 * 24 * 60 * 60
)

// ScheduleKind is the kind of a schedule, for records and filters.
type ScheduleKind string

const (
	Manual    ScheduleKind = "manual"
	Recurring ScheduleKind = "recurring"
	Once      ScheduleKind = "once"
)

// Schedule is when a job runs: manual only, a recurring cron expression in
// the daemon's local time, or once at RunAt.
type Schedule struct {
	Kind ScheduleKind
	// Cron is the expression as written (Recurring).
	Cron string
	// RunAt keeps the offset it was written with (Once).
	RunAt time.Time
	cron  *Cron
}

// ParseSchedule builds the schedule from the `schedule` and `run_at` keys.
func ParseSchedule(cron, runAt *string) (Schedule, error) {
	switch {
	case cron != nil && runAt != nil:
		return Schedule{}, fmt.Errorf("set either `schedule` (recurring) or `run_at` (once), not both")
	case cron != nil:
		parsed, err := ParseCron(*cron)
		if err != nil {
			return Schedule{}, err
		}
		return Schedule{Kind: Recurring, Cron: *cron, cron: parsed}, nil
	case runAt != nil:
		at, err := parseRunAt(*runAt)
		if err != nil {
			return Schedule{}, err
		}
		return Schedule{Kind: Once, RunAt: at}, nil
	}
	return Schedule{Kind: Manual}, nil
}

// parseRunAt accepts what chrono's RFC 3339 parser, which this replaces,
// accepted: `T`, `t`, or a space between date and time, and `Z`, `z`, or a
// `±HH:MM` offset (hours up to 23, minutes up to 59).
func parseRunAt(raw string) (time.Time, error) {
	invalid := fmt.Errorf("invalid run_at '%s': use RFC 3339 with an explicit offset, for example 2026-10-20T09:00:00-03:00 or 2026-10-20T12:00:00Z", raw)
	b := []byte(strings.TrimSpace(raw))
	if len(b) < 20 {
		return time.Time{}, invalid
	}
	if b[10] == 't' || b[10] == ' ' {
		b[10] = 'T'
	}
	if b[len(b)-1] == 'z' {
		b[len(b)-1] = 'Z'
	}
	value := string(b)
	if !validOffset(value) {
		return time.Time{}, invalid
	}
	at, err := time.Parse(time.RFC3339Nano, value)
	if err != nil {
		return time.Time{}, invalid
	}
	if at.Nanosecond() != 0 {
		return time.Time{}, fmt.Errorf("invalid run_at '%s': use whole seconds", raw)
	}
	return at, nil
}

func validOffset(v string) bool {
	if strings.HasSuffix(v, "Z") {
		return true
	}
	tail := v[len(v)-6:]
	digits := func(s string) (int, bool) {
		n, err := strconv.Atoi(s)
		return n, err == nil && len(s) == 2 && s[0] >= '0' && s[0] <= '9'
	}
	hours, okH := digits(tail[1:3])
	minutes, okM := digits(tail[4:6])
	return (tail[0] == '+' || tail[0] == '-') && tail[3] == ':' && okH && okM && hours <= 23 && minutes <= 59
}

// Describe is the short label: the cron expression, `once <run_at>`, or
// `manual`.
func (s Schedule) Describe() string {
	switch s.Kind {
	case Recurring:
		return s.Cron
	case Once:
		return "once " + offsetRFC3339(s.RunAt)
	}
	return "manual"
}

// DefaultGraceSecs applies when grace_secs is not declared; false for manual
// jobs, which are never scheduled.
func (s Schedule) DefaultGraceSecs() (uint32, bool) {
	switch s.Kind {
	case Recurring:
		return DefaultRecurringGraceSecs, true
	case Once:
		return DefaultOnceGraceSecs, true
	}
	return 0, false
}

// MarshalJSON writes `{"kind": ...}` with `cron` or `run_at` (its own
// offset, `Z` for UTC, as chrono's serde does).
func (s Schedule) MarshalJSON() ([]byte, error) {
	switch s.Kind {
	case Recurring:
		return json.Marshal(struct {
			Kind ScheduleKind `json:"kind"`
			Cron string       `json:"cron"`
		}{s.Kind, s.Cron})
	case Once:
		return json.Marshal(struct {
			Kind  ScheduleKind `json:"kind"`
			RunAt string       `json:"run_at"`
		}{s.Kind, s.RunAt.Format(time.RFC3339)})
	}
	return json.Marshal(struct {
		Kind ScheduleKind `json:"kind"`
	}{Manual})
}

// Occurrence is one scheduled run of a job.
type Occurrence struct {
	At time.Time
	// Key is the occurrence's identity: the UTC instant for once jobs, the
	// local wall-clock minute for recurring jobs.
	Key string
}

// OnceOccurrence is the single occurrence of a once job.
func OnceOccurrence(runAt time.Time) Occurrence {
	at := runAt.UTC()
	return Occurrence{At: at, Key: at.Format("20060102T150405Z")}
}

// RecurringOccurrences lists occurrences of c in the UTC window (after,
// until], matched against local time in loc, oldest first, at most one per
// local minute key.
func RecurringOccurrences(c *Cron, after, until time.Time, loc *time.Location) []Occurrence {
	var out []Occurrence
	seen := map[string]bool{}
	for at := floorMinute(after).Add(time.Minute); !at.After(until); at = at.Add(time.Minute) {
		local := at.In(loc)
		if c.Matches(local) {
			key := local.Format("20060102T1504")
			if !seen[key] {
				seen[key] = true
				out = append(out, Occurrence{At: at, Key: key})
			}
		}
	}
	return out
}

// NextRecurringAfter is the next occurrence of c strictly after after,
// looking up to 367 days ahead; false when it never matches in that span.
func NextRecurringAfter(c *Cron, after time.Time, loc *time.Location) (time.Time, bool) {
	limit := after.Add(367 * 24 * time.Hour)
	at := floorMinute(after).Add(time.Minute)
	for !at.After(limit) {
		local := at.In(loc)
		if !c.matchesDay(local) || !c.matchesHour(local) {
			// Step to the next local hour boundary. Offsets change by whole
			// hours here, so no matching minute is skipped.
			at = at.Add(time.Duration(60-local.Minute()) * time.Minute)
			continue
		}
		if c.Matches(local) {
			return at, true
		}
		at = at.Add(time.Minute)
	}
	return time.Time{}, false
}

// floorMinute drops seconds and fractions (whole Unix seconds first, as
// chrono's timestamp() does).
func floorMinute(t time.Time) time.Time {
	ts := t.Unix()
	m := ts % 60
	if m < 0 {
		m += 60
	}
	return time.Unix(ts-m, 0).UTC()
}

// Cron is a parsed 5-field cron expression: minute hour day-of-month month
// day-of-week. Each field accepts `*`, `N`, `*/N`, `A-B`, and comma lists of
// those. Day-of-week is 0-6 (Sunday is 0; 7 is also Sunday). Day-of-month and
// day-of-week must both match.
type Cron struct {
	minutes, hours, days, months, weekdays uint64
}

// ParseCron parses a 5-field expression.
func ParseCron(expr string) (*Cron, error) {
	fields := strings.Fields(expr)
	if len(fields) != 5 {
		return nil, fmt.Errorf("Invalid schedule '%s': expected 5 cron fields (minute hour day month weekday)", expr)
	}
	var c Cron
	targets := []*uint64{&c.minutes, &c.hours, &c.days, &c.months, &c.weekdays}
	bounds := [][2]uint32{{0, 59}, {0, 23}, {1, 31}, {1, 12}, {0, 6}}
	for i, field := range fields {
		mask, err := parseField(field, bounds[i][0], bounds[i][1], i == 4)
		if err != nil {
			return nil, fmt.Errorf("Invalid schedule '%s': %w", expr, err)
		}
		*targets[i] = mask
	}
	return &c, nil
}

// Matches reports whether the expression matches this local minute.
func (c *Cron) Matches(local time.Time) bool {
	return c.matchesDay(local) && c.matchesHour(local) && bit(c.minutes, local.Minute())
}

func (c *Cron) matchesDay(local time.Time) bool {
	return bit(c.days, local.Day()) && bit(c.months, int(local.Month())) && bit(c.weekdays, int(local.Weekday()))
}

func (c *Cron) matchesHour(local time.Time) bool { return bit(c.hours, local.Hour()) }

func bit(mask uint64, v int) bool { return v >= 0 && v < 64 && mask&(1<<uint(v)) != 0 }

func parseField(expr string, min, max uint32, dow bool) (uint64, error) {
	// Day-of-week accepts 7 as Sunday in single values and ranges (`1-7`);
	// it is folded onto 0 after expansion.
	valueMax := max
	if dow {
		valueMax = 7
	}
	var mask uint64
	var parts []string
	for _, p := range strings.Split(expr, ",") {
		parts = append(parts, strings.TrimSpace(p))
	}
	empty := true
	for _, p := range parts {
		if p != "" {
			empty = false
		}
	}
	if empty {
		return 0, fmt.Errorf("Invalid empty cron field")
	}
	for _, part := range parts {
		switch {
		case part == "":
		case part == "*":
			for v := min; v <= max; v++ {
				mask |= 1 << v
			}
		case strings.HasPrefix(part, "*/"):
			step, err := parseValue(part[2:], 1, valueMax)
			if err != nil {
				return 0, err
			}
			for v := min; v <= max; v += step {
				mask |= 1 << v
			}
		case strings.Contains(part, "-"):
			a, b, _ := strings.Cut(part, "-")
			start, err := parseValue(a, min, valueMax)
			if err != nil {
				return 0, err
			}
			end, err := parseValue(b, min, valueMax)
			if err != nil {
				return 0, err
			}
			if start > end {
				return 0, fmt.Errorf("Invalid range '%s': start must be <= end", part)
			}
			for v := start; v <= end; v++ {
				mask |= 1 << v
			}
		default:
			v, err := parseValue(part, min, valueMax)
			if err != nil {
				return 0, err
			}
			mask |= 1 << v
		}
	}
	if dow && mask&(1<<7) != 0 {
		mask = mask&^(1<<7) | 1
	}
	return mask, nil
}

func parseValue(raw string, min, max uint32) (uint32, error) {
	t := strings.TrimSpace(raw)
	if t == "" {
		return 0, fmt.Errorf("Invalid empty cron value")
	}
	// One leading `+` is accepted, as the Rust parser this replaces did.
	v, err := strconv.ParseUint(strings.TrimPrefix(t, "+"), 10, 32)
	if err != nil {
		return 0, fmt.Errorf("Invalid cron value '%s'", t)
	}
	if uint32(v) < min || uint32(v) > max {
		return 0, fmt.Errorf("Cron value '%s' out of range %d..%d", t, min, max)
	}
	return uint32(v), nil
}
