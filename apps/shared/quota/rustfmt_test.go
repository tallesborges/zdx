package quota

import (
	"math"
	"strings"
	"testing"
	"time"
)

func TestParseRFC3339MatchesChrono(t *testing.T) {
	ok := map[string]string{
		"2026-07-14T01:09:59.535017+00:00": "2026-07-14T01:09:59.535017+00:00",
		"2026-07-21T03:22:39Z":             "2026-07-21T03:22:39+00:00",
		"2026-07-21t03:22:39z":             "2026-07-21T03:22:39+00:00",
		"2026-07-21 03:22:39Z":             "2026-07-21T03:22:39+00:00",
		"2026-08-14T15:10:54.202Z":         "2026-08-14T15:10:54.202+00:00",
		"2026-08-14T15:10:54.1234567891Z":  "2026-08-14T15:10:54.123456789+00:00",
		"2026-08-14T15:10:54.5000Z":        "2026-08-14T15:10:54.500+00:00",
		"2026-08-14T05:30:00+05:30":        "2026-08-14T00:00:00+00:00",
		"2026-08-14T00:00:00\u221201:00":   "2026-08-14T01:00:00+00:00",
		"2026-08-14T00:00:00-23:59":        "2026-08-14T23:59:00+00:00",
		"2024-02-29T00:00:00Z":             "2024-02-29T00:00:00+00:00",
		"0000-01-01T00:00:00+01:00":        "-0001-12-31T23:00:00+00:00",
	}
	for in, want := range ok {
		got := parseRFC3339(str(in))
		if got == nil {
			t.Errorf("%q: nil", in)
			continue
		}
		if s := FormatRFC3339(*got); s != want {
			t.Errorf("%q = %s, want %s", in, s, want)
		}
	}
	for _, in := range []string{
		"", "2026-07-21", "2026-07-21T03:22:39", "2026-07-21T03:22:39+0000", "2026-07-21T03:22:39+00",
		"2026-07-21T03:22:39+24:00", "2026-07-21T03:22:39+00:60", "2026-02-30T00:00:00Z", "2025-02-29T00:00:00Z",
		"2026-07-21T24:00:00Z", "2026-07-21T03:61:00Z", "2026-07-21T03:22:61Z", "2026-07-21T03:22:39.Z",
		"2026-07-21T03:22:39Z ", " 2026-07-21T03:22:39Z", "2026-07-21X03:22:39Z", "+2026-07-21T03:22:39Z",
		"2026-07-21T03:22:39UTC", "2026-07-21T03:22:39.5+00:00:00",
	} {
		if got := parseRFC3339(str(in)); got != nil {
			t.Errorf("%q = %v, want nil", in, got)
		}
	}
	// Second 60 is kept as chrono keeps it, at the end of any minute and
	// with any fraction or offset; the instant is the next second.
	for in, want := range map[string]string{
		"2016-12-31T23:59:60Z":        "2016-12-31T23:59:60+00:00",
		"2016-12-31T23:59:60.25Z":     "2016-12-31T23:59:60.250+00:00",
		"2026-07-21T03:22:60+01:00":   "2026-07-21T02:22:60+00:00",
		"9999-12-31T23:59:60.000001Z": "9999-12-31T23:59:60.000001+00:00",
	} {
		got := parseRFC3339(str(in))
		if got == nil || FormatRFC3339(*got) != want {
			t.Errorf("%q = %v", in, got)
			continue
		}
		if next := mustTime(t, strings.Replace(want, ":60", ":59", 1)).Add(time.Second); !got.Equal(next) {
			t.Errorf("%q instant = %v, want %v", in, got, next)
		}
		if FormatRFC3339(got.UTC()) == want {
			t.Errorf("%q: UTC() should drop the mark", in)
		}
	}
}

func TestFormatRFC3339AutoSi(t *testing.T) {
	base := time.Date(2026, 8, 22, 10, 0, 0, 0, time.UTC)
	for nanos, want := range map[int]string{
		0:           "2026-08-22T10:00:00+00:00",
		500_000_000: "2026-08-22T10:00:00.500+00:00",
		535_017_000: "2026-08-22T10:00:00.535017+00:00",
		1:           "2026-08-22T10:00:00.000000001+00:00",
		100_000:     "2026-08-22T10:00:00.000100+00:00",
	} {
		if got := FormatRFC3339(base.Add(time.Duration(nanos))); got != want {
			t.Errorf("%d = %s, want %s", nanos, got, want)
		}
	}
	if got := FormatRFC3339(time.Date(2026, 1, 1, 3, 0, 0, 0, time.FixedZone("x", 3600))); got != "2026-01-01T02:00:00+00:00" {
		t.Errorf("offset = %s", got)
	}
	if got := FormatRFC3339(time.Date(10000, 1, 2, 0, 0, 0, 0, time.UTC)); got != "+10000-01-02T00:00:00+00:00" {
		t.Errorf("big year = %s", got)
	}
}

func TestTimestampRangeMatchesChrono(t *testing.T) {
	if timestamp(maxTimestamp) == nil || timestamp(maxTimestamp+1) != nil {
		t.Error("max bound")
	}
	if timestamp(minTimestamp) == nil || timestamp(minTimestamp-1) != nil {
		t.Error("min bound")
	}
	if got := FormatRFC3339(*timestamp(maxTimestamp)); got != "+262142-12-31T23:59:59+00:00" {
		t.Errorf("max = %s", got)
	}
	if got := FormatRFC3339(*timestamp(minTimestamp)); got != "-262143-01-01T00:00:00+00:00" {
		t.Errorf("min = %s", got)
	}
}

func TestJSONFloatMatchesSerdeJSON(t *testing.T) {
	for v, want := range map[float64]string{
		0:                     "0.0",
		6:                     "6.0",
		42:                    "42.0",
		12.5:                  "12.5",
		100:                   "100.0",
		0.29:                  "0.29",
		1e15:                  "1000000000000000.0",
		1e16:                  "1e+16",
		1.5e16:                "1.5e+16",
		123456789012345680000: "1.2345678901234568e+20",
		0.00001:               "0.00001",
		0.000012345:           "0.000012345",
		0.000001:              "1e-6",
		1.5e-7:                "1.5e-7",
		-3.25:                 "-3.25",
		0.30000000000000004:   "0.30000000000000004",
		1e300:                 "1e+300",
		5e-324:                "5e-324",
	} {
		if got := jsonFloat(v); got != want {
			t.Errorf("%v = %s, want %s", v, got, want)
		}
	}
	for v, want := range map[float64]string{math.Copysign(0, -1): "-0.0", math.NaN(): "null", math.Inf(1): "null", math.Inf(-1): "null"} {
		if got := jsonFloat(v); got != want {
			t.Errorf("%v = %s, want %s", v, got, want)
		}
	}
}

func TestRustFixedAndFloatParse(t *testing.T) {
	for _, c := range []struct {
		v    float64
		prec int
		want string
	}{{0.5, 0, "0"}, {1.5, 0, "2"}, {2.5, 0, "2"}, {99.5, 0, "100"}, {0.29, 0, "0"}, {-0.3, 0, "-0"}, {0.893049955368042, 2, "0.89"}, {2500, 2, "2500.00"}, {0.125, 2, "0.12"}, {math.Inf(1), 2, "inf"}, {math.Inf(-1), 0, "-inf"}, {math.NaN(), 0, "NaN"}} {
		if got := rustFixed(c.v, c.prec); got != c.want {
			t.Errorf("%v %d = %s, want %s", c.v, c.prec, got, c.want)
		}
	}
	good := map[string]float64{"2500": 2500, "0.893049955368042": 0.893049955368042, "+1": 1, "1.": 1, ".5": 0.5, "1e3": 1000, "1E-2": 0.01, "-0": math.Copysign(0, -1)}
	for in, want := range good {
		got, ok := parseRustFloat(in)
		if !ok || got != want || math.Signbit(got) != math.Signbit(want) {
			t.Errorf("%q = %v %v", in, got, ok)
		}
	}
	for _, in := range []string{"inf", "-Infinity", "+INF"} {
		if v, ok := parseRustFloat(in); !ok || !math.IsInf(v, 0) {
			t.Errorf("%q = %v %v", in, v, ok)
		}
	}
	for _, in := range []string{"nan", "-NaN", "+nan"} {
		if v, ok := parseRustFloat(in); !ok || !math.IsNaN(v) {
			t.Errorf("%q = %v %v", in, v, ok)
		}
	}
	if v, ok := parseRustFloat("1e400"); !ok || !math.IsInf(v, 1) {
		t.Errorf("1e400 = %v %v", v, ok)
	}
	for _, in := range []string{"", ".", " 5", "5 ", "1_000", "0x10", "0x1p-2", "1e", "e5", "++1", "1,5", "infinit"} {
		if _, ok := parseRustFloat(in); ok {
			t.Errorf("%q parsed", in)
		}
	}
}

func TestNumberKeepsSerdeClassification(t *testing.T) {
	cases := []struct {
		in   string
		kind numKind
		f    float64
		fail bool
	}{
		{in: "6", kind: numU64, f: 6},
		{in: "-6", kind: numI64, f: -6},
		{in: "-0", kind: numF64, f: math.Copysign(0, -1)},
		{in: "6.0", kind: numF64, f: 6},
		{in: "1e3", kind: numF64, f: 1000},
		{in: "18446744073709551615", kind: numU64, f: 18446744073709551615},
		{in: "18446744073709551616", kind: numF64, f: 18446744073709551616},
		{in: "0.9971372", kind: numF64, f: 0.9971372},
		{in: "1e-400", kind: numF64, f: 0},
		{in: "1e400", fail: true},
		{in: "1e99999999999", fail: true},
		{in: "0e99999999999", kind: numF64, f: 0},
		{in: "1e-99999999999", kind: numF64, f: 0},
	}
	for _, c := range cases {
		n, err := number([]byte(c.in))
		if c.fail {
			if err == nil {
				t.Errorf("%s: want error", c.in)
			}
			continue
		}
		if err != nil || n.kind != c.kind || n.float() != c.f || math.Signbit(n.float()) != math.Signbit(c.f) {
			t.Errorf("%s = %+v %v", c.in, n, err)
		}
	}
	// Once the significand would pass u64, serde_json (no float_roundtrip)
	// drops the remaining digits and scales by a power of ten.
	n, _ := number([]byte("123456789012345678901"))
	if want := float64(uint64(12345678901234567890)) * 10; n.float() != want {
		t.Errorf("long integer = %v, want %v", n.float(), want)
	}
	n, _ = number([]byte("0.12345678901234567890123"))
	if want := float64(uint64(12345678901234567890)) / 1e20; n.float() != want {
		t.Errorf("long decimal = %v, want %v", n.float(), want)
	}
}
