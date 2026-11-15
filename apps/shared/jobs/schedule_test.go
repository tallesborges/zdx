package jobs

import (
	"strings"
	"testing"
	"time"
	_ "time/tzdata"
)

func utc(y int, mo time.Month, d, h, mi int) time.Time {
	return time.Date(y, mo, d, h, mi, 0, 0, time.UTC)
}

func mustCron(t *testing.T, expr string) *Cron {
	t.Helper()
	c, err := ParseCron(expr)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestCronMatches(t *testing.T) {
	local := func(y int, mo time.Month, d, h, mi int) time.Time { return time.Date(y, mo, d, h, mi, 0, 0, time.UTC) }
	cases := []struct {
		expr string
		at   time.Time
		want bool
	}{
		{"0 8 * * *", local(2026, 2, 11, 8, 0), true},
		{"0 8 * * *", local(2026, 2, 11, 8, 1), false},
		{"*/15 8-10 * * *", local(2026, 2, 11, 8, 30), true},
		{"*/20 8-10 * * *", local(2026, 2, 11, 8, 30), false},
		{"0 9 * * 7", local(2026, 10, 18, 9, 0), true},
		{"0 9 * * 1-7", local(2026, 10, 18, 9, 0), true},
		{"0 9 * * 1-7", local(2026, 10, 19, 9, 0), true},
		{"0 9 * * 0-7", local(2026, 10, 21, 9, 0), true},
		{"0,30 9 1,15 * *", local(2026, 10, 15, 9, 30), true},
		{"0 9 * * +1", local(2026, 10, 19, 9, 0), true},
	}
	for _, c := range cases {
		if got := mustCron(t, c.expr).Matches(c.at); got != c.want {
			t.Errorf("%s at %v: %v", c.expr, c.at, got)
		}
	}
}

func TestCronRejectsBadExpressions(t *testing.T) {
	for expr, want := range map[string]string{
		"0 8 * *":     "expected 5 cron fields",
		"60 8 * * *":  "out of range",
		"*/0 * * * *": "out of range",
		"5-1 * * * *": "start must be <= end",
		"x * * * *":   "Invalid cron value",
		", * * * *":   "Invalid empty cron field",
	} {
		_, err := ParseCron(expr)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%s: %v", expr, err)
		}
	}
}

func TestOnceKeyIsTheUTCInstantAndDescribeKeepsTheOffset(t *testing.T) {
	s, err := ParseSchedule(nil, ptr("2026-10-20T09:00:00-03:00"))
	if err != nil {
		t.Fatal(err)
	}
	occ := OnceOccurrence(s.RunAt)
	if occ.Key != "20261020T120000Z" || !occ.At.Equal(utc(2026, 10, 20, 12, 0)) {
		t.Fatalf("%+v", occ)
	}
	if s.Describe() != "once 2026-10-20T09:00:00-03:00" {
		t.Fatal(s.Describe())
	}
	z, _ := ParseSchedule(nil, ptr("2026-10-20T12:00:00Z"))
	if z.Describe() != "once 2026-10-20T12:00:00+00:00" {
		t.Fatal(z.Describe())
	}
	b, _ := z.MarshalJSON()
	if string(b) != `{"kind":"once","run_at":"2026-10-20T12:00:00Z"}` {
		t.Fatal(string(b))
	}
}

func TestRunAtAcceptsWhatChronoAccepted(t *testing.T) {
	want := utc(2030, 1, 1, 9, 0)
	for _, raw := range []string{"2030-01-01T09:00:00Z", "2030-01-01t09:00:00z", "2030-01-01 09:00:00Z", "2030-01-01T06:00:00-03:00", "2030-01-01T09:00:00.000Z"} {
		if at, err := parseRunAt(raw); err != nil || !at.Equal(want) {
			t.Errorf("%s: %v %v", raw, at, err)
		}
	}
	for _, raw := range []string{"2030-01-01T09:00:00", "2030-01-01T09:00:00+00:60", "2030-01-01T09:00:00+24:00", "2030-01-01T09:00:00+0000", "2030-01-01"} {
		if _, err := parseRunAt(raw); err == nil {
			t.Errorf("%s accepted", raw)
		}
	}
}

func TestOccurrencesCoverAHalfOpenWindowInLocalTime(t *testing.T) {
	loc := time.FixedZone("", -3*3600)
	found := RecurringOccurrences(mustCron(t, "*/30 9 * * *"), utc(2026, 10, 20, 12, 0), utc(2026, 10, 20, 13, 0), loc)
	if len(found) != 1 || found[0].Key != "20261020T0930" || !found[0].At.Equal(utc(2026, 10, 20, 12, 30)) {
		t.Fatalf("%+v", found)
	}
}

func saoPaulo(t *testing.T) *time.Location {
	loc, err := time.LoadLocation("America/Sao_Paulo")
	if err != nil {
		t.Fatal(err)
	}
	return loc
}

func TestDSTFallBackRunsARepeatedLocalMinuteOnce(t *testing.T) {
	// 2019-02-17 00:00 local (-02) became 2019-02-16 23:00 (-03): local
	// 23:00-23:59 on the 16th happened twice.
	found := RecurringOccurrences(mustCron(t, "30 23 * * *"), utc(2019, 2, 17, 0, 0), utc(2019, 2, 17, 4, 0), saoPaulo(t))
	if len(found) != 1 || found[0].Key != "20190216T2330" {
		t.Fatalf("%+v", found)
	}
}

func TestDSTSpringForwardSkipsAMissingLocalMinute(t *testing.T) {
	// 2018-11-04 00:00 local (-03) became 01:00 (-02): local 00:00-00:59
	// did not exist that day.
	loc := saoPaulo(t)
	c := mustCron(t, "30 0 * * *")
	if found := RecurringOccurrences(c, utc(2018, 11, 4, 0, 0), utc(2018, 11, 4, 6, 0), loc); len(found) != 0 {
		t.Fatalf("%+v", found)
	}
	next, ok := NextRecurringAfter(c, utc(2018, 11, 4, 0, 0), loc)
	if !ok || !next.Equal(utc(2018, 11, 5, 2, 30)) {
		t.Fatal(next)
	}
}

func TestNextOccurrenceSkipsAheadAndGivesUpOnImpossibleDates(t *testing.T) {
	loc := time.FixedZone("", -3*3600)
	next, ok := NextRecurringAfter(mustCron(t, "0 9 * * 1"), utc(2026, 10, 13, 12, 0), loc)
	if !ok || !next.Equal(utc(2026, 10, 19, 12, 0)) {
		t.Fatal(next)
	}
	if _, ok := NextRecurringAfter(mustCron(t, "0 9 30 2 *"), utc(2026, 10, 13, 12, 0), loc); ok {
		t.Fatal("Feb 30 never matches")
	}
}

func TestOffsetRFC3339MatchesChrono(t *testing.T) {
	for in, want := range map[time.Time]string{
		time.Date(2026, 11, 15, 8, 30, 0, 0, time.UTC):         "2026-11-15T08:30:00+00:00",
		time.Date(2026, 11, 15, 8, 30, 0, 449929000, time.UTC): "2026-11-15T08:30:00.449929+00:00",
		time.Date(2026, 11, 15, 8, 30, 0, 500000000, time.UTC): "2026-11-15T08:30:00.500+00:00",
		time.Date(2026, 11, 15, 8, 30, 0, 123456789, time.UTC): "2026-11-15T08:30:00.123456789+00:00",
	} {
		if got := offsetRFC3339(in); got != want {
			t.Errorf("%v: %s", in, got)
		}
	}
}
