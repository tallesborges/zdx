package quota

import (
	"math"
	"strconv"
	"strings"
	"time"
)

// Ports of the Rust formatting and parsing the quota readers relied on:
// chrono's RFC 3339 parser and `to_rfc3339`, chrono's timestamp range,
// serde_json's float text, Rust's `f64::from_str`, and `{:.N}` formatting.

// chrono's NaiveDate range: years -262143 through 262142.
var (
	minTimestamp = time.Date(-262143, time.January, 1, 0, 0, 0, 0, time.UTC).Unix()
	maxTimestamp = time.Date(262142, time.December, 31, 23, 59, 59, 0, time.UTC).Unix()
)

// timestamp is chrono's `Utc.timestamp_opt(secs, 0).single()`.
func timestamp(secs int64) *time.Time {
	if secs < minTimestamp || secs > maxTimestamp {
		return nil
	}
	t := time.Unix(secs, 0).UTC()
	return &t
}

// leapSecond marks an instant chrono kept as a leap second (`:60`).
// time.Time cannot hold second 60, so the instant is stored as the next
// second in this UTC location, and FormatRFC3339 prints it back as `:60`.
// Plain time arithmetic on it can be one second off chrono's (the CLI
// countdown corrects for that in chronoSeconds). Converting it with UTC()
// or In() drops the mark.
var leapSecond = time.FixedZone("UTC", 0)

// parseRFC3339 is chrono's `DateTime::parse_from_rfc3339` converted to UTC;
// nil where chrono returns an error. Second 60 is accepted at the end of any
// minute, as chrono does, and kept with the leapSecond mark.
func parseRFC3339(p *string) *time.Time {
	if p == nil {
		return nil
	}
	b := []byte(*p)
	if len(b) < 19 {
		return nil
	}
	digit := func(i int) (int, bool) {
		if b[i] < '0' || b[i] > '9' {
			return 0, false
		}
		return int(b[i] - '0'), true
	}
	two := func(i int) (int, bool) {
		a, ok1 := digit(i)
		c, ok2 := digit(i + 1)
		return a*10 + c, ok1 && ok2
	}
	y1, ok1 := two(0)
	y2, ok2 := two(2)
	if !ok1 || !ok2 || b[4] != '-' {
		return nil
	}
	year := y1*100 + y2
	month, ok := two(5)
	if !ok || b[7] != '-' {
		return nil
	}
	day, ok := two(8)
	if !ok || month < 1 || month > 12 || day < 1 || day > daysIn(year, month) {
		return nil
	}
	if b[10] != 't' && b[10] != 'T' && b[10] != ' ' {
		return nil
	}
	hour, ok := two(11)
	if !ok || b[13] != ':' {
		return nil
	}
	minute, ok := two(14)
	if !ok || b[16] != ':' {
		return nil
	}
	sec, ok := two(17)
	if !ok {
		return nil
	}
	extra := 0
	if sec == 60 {
		sec, extra = 59, 1_000_000_000
	}
	rest := b[19:]
	nano := extra
	if len(b) > 19 && b[19] == '.' {
		frac := b[20:]
		n, v := 0, 0
		for n < len(frac) && n < 9 && frac[n] >= '0' && frac[n] <= '9' {
			v = v*10 + int(frac[n]-'0')
			n++
		}
		if n == 0 {
			return nil
		}
		for i := n; i < 9; i++ {
			v *= 10
		}
		for n < len(frac) && frac[n] >= '0' && frac[n] <= '9' {
			n++
		}
		nano += v
		rest = frac[n:]
	}
	if hour > 23 || minute > 59 || sec > 59 {
		return nil
	}
	offset, ok := rfc3339Offset(rest)
	if !ok || offset <= -86_400 || offset >= 86_400 {
		return nil
	}
	t := time.Date(year, time.Month(month), day, hour, minute, sec, nano, time.UTC).Add(-time.Duration(offset) * time.Second)
	if extra != 0 {
		t = t.In(leapSecond)
	}
	return &t
}

// rfc3339Offset is chrono's `timezone_offset` with a required colon, Zulu
// allowed, and U+2212 accepted as a minus sign. The whole input must be used.
func rfc3339Offset(s []byte) (int, bool) {
	if len(s) == 0 {
		return 0, false
	}
	if s[0] == 'Z' || s[0] == 'z' {
		return 0, len(s) == 1
	}
	negative := false
	switch {
	case s[0] == '+':
		s = s[1:]
	case s[0] == '-':
		negative, s = true, s[1:]
	case strings.HasPrefix(string(s), "\u2212"):
		negative, s = true, s[len("\u2212"):]
	default:
		return 0, false
	}
	if len(s) < 2 || s[0] < '0' || s[0] > '9' || s[1] < '0' || s[1] > '9' {
		return 0, false
	}
	hours := int(s[0]-'0')*10 + int(s[1]-'0')
	s = s[2:]
	if len(s) == 0 || s[0] != ':' {
		return 0, false
	}
	s = s[1:]
	if len(s) < 2 || s[0] < '0' || s[0] > '5' || s[1] < '0' || s[1] > '9' {
		return 0, false
	}
	minutes := int(s[0]-'0')*10 + int(s[1]-'0')
	if len(s) != 2 {
		return 0, false
	}
	secs := hours*3600 + minutes*60
	if negative {
		secs = -secs
	}
	return secs, true
}

func daysIn(year, month int) int {
	switch month {
	case 2:
		if year%4 == 0 && (year%100 != 0 || year%400 == 0) {
			return 29
		}
		return 28
	case 4, 6, 9, 11:
		return 30
	}
	return 31
}

// FormatRFC3339 formats an instant as chrono's `DateTime<Utc>::to_rfc3339`:
// UTC, `+00:00`, and fractional seconds only when non-zero, in 3, 6, or 9
// digits. A reset parsed from a leap second prints as `:60`, as chrono does.
func FormatRFC3339(t time.Time) string {
	leap := t.Location() == leapSecond
	t = t.UTC()
	if leap {
		t = t.Add(-time.Second)
	}
	var b strings.Builder
	year := t.Year()
	if year >= 0 && year <= 9999 {
		b.WriteString(pad(year, 4))
	} else {
		if year < 0 {
			b.WriteByte('-')
			year = -year
		} else {
			b.WriteByte('+')
		}
		b.WriteString(pad(year, 4))
	}
	b.WriteByte('-')
	b.WriteString(pad(int(t.Month()), 2))
	b.WriteByte('-')
	b.WriteString(pad(t.Day(), 2))
	b.WriteByte('T')
	b.WriteString(pad(t.Hour(), 2))
	b.WriteByte(':')
	b.WriteString(pad(t.Minute(), 2))
	b.WriteByte(':')
	if leap {
		b.WriteString("60")
	} else {
		b.WriteString(pad(t.Second(), 2))
	}
	switch nano := t.Nanosecond(); {
	case nano == 0:
	case nano%1_000_000 == 0:
		b.WriteString("." + pad(nano/1_000_000, 3))
	case nano%1_000 == 0:
		b.WriteString("." + pad(nano/1_000, 6))
	default:
		b.WriteString("." + pad(nano, 9))
	}
	b.WriteString("+00:00")
	return b.String()
}

func pad(v, width int) string {
	s := strconv.Itoa(v)
	for len(s) < width {
		s = "0" + s
	}
	return s
}

// jsonFloat is serde_json's text for an f64 (zmij): shortest round-trip
// digits, plain notation for decimal exponents -5..=15 with a `.0` on whole
// numbers, `1.5e+16` style outside, and `null` for NaN and infinities.
func jsonFloat(v float64) string {
	if math.IsNaN(v) || math.IsInf(v, 0) {
		return "null"
	}
	sign := ""
	if math.Signbit(v) {
		sign = "-"
		v = -v
	}
	if v == 0 {
		return sign + "0.0"
	}
	e := strconv.FormatFloat(v, 'e', -1, 64)
	mant, expText, _ := strings.Cut(e, "e")
	digits := strings.Replace(mant, ".", "", 1)
	exp, _ := strconv.Atoi(expText)
	n := len(digits)
	if exp >= -5 && exp <= 15 {
		switch {
		case n-1 <= exp:
			return sign + digits + strings.Repeat("0", exp+1-n) + ".0"
		case exp >= 0:
			return sign + digits[:exp+1] + "." + digits[exp+1:]
		default:
			return sign + "0." + strings.Repeat("0", -exp-1) + digits
		}
	}
	out := sign + digits[:1]
	if n > 1 {
		out += "." + digits[1:]
	}
	if exp >= 0 {
		return out + "e+" + strconv.Itoa(exp)
	}
	return out + "e-" + strconv.Itoa(-exp)
}

// rustFixed is Rust's `format!("{:.prec$}", v)`.
func rustFixed(v float64, prec int) string {
	switch {
	case math.IsNaN(v):
		return "NaN"
	case math.IsInf(v, 1):
		return "inf"
	case math.IsInf(v, -1):
		return "-inf"
	}
	return strconv.FormatFloat(v, 'f', prec, 64)
}

// parseRustFloat is Rust's `str::parse::<f64>()`: an optional sign, then
// `inf`, `infinity` or `nan` in any case, or decimal digits with an optional
// fraction and exponent. No whitespace, hex, or underscores.
func parseRustFloat(s string) (float64, bool) {
	body := s
	if body != "" && (body[0] == '+' || body[0] == '-') {
		body = body[1:]
	}
	switch strings.ToLower(body) {
	case "inf", "infinity":
		if s[0] == '-' {
			return math.Inf(-1), true
		}
		return math.Inf(1), true
	case "nan":
		return math.NaN(), true
	}
	i, digits := 0, 0
	for i < len(body) && body[i] >= '0' && body[i] <= '9' {
		i++
		digits++
	}
	if i < len(body) && body[i] == '.' {
		i++
		for i < len(body) && body[i] >= '0' && body[i] <= '9' {
			i++
			digits++
		}
	}
	if digits == 0 {
		return 0, false
	}
	if i < len(body) && (body[i] == 'e' || body[i] == 'E') {
		i++
		if i < len(body) && (body[i] == '+' || body[i] == '-') {
			i++
		}
		start := i
		for i < len(body) && body[i] >= '0' && body[i] <= '9' {
			i++
		}
		if i == start {
			return 0, false
		}
	}
	if i != len(body) {
		return 0, false
	}
	v, err := strconv.ParseFloat(s, 64)
	if err != nil {
		if ne, ok := err.(*strconv.NumError); ok && ne.Err == strconv.ErrRange {
			return v, true
		}
		return 0, false
	}
	return v, true
}

// saturatingInt64 is Rust's `f as i64`: truncation toward zero, saturating
// at the i64 bounds, NaN as 0.
func saturatingInt64(f float64) int64 {
	switch {
	case math.IsNaN(f):
		return 0
	case f >= math.MaxInt64:
		return math.MaxInt64
	case f <= math.MinInt64:
		return math.MinInt64
	}
	return int64(f)
}
