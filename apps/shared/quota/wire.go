package quota

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"unicode/utf8"
)

// The helpers below decode JSON the way the Rust readers did with serde
// derive and serde_json::from_slice (default features), for the inputs the
// differential fixtures and the tests pin:
//   - field names are case-sensitive; unknown fields are skipped unchecked,
//     as serde's IgnoredAny (invalid UTF-8, lone surrogates, out-of-range
//     numbers, and deep nesting inside them are accepted, as in Rust);
//   - every key of a decoded object and every string read into a field must
//     be valid UTF-8 with paired surrogate escapes;
//   - a repeated declared field is an error;
//   - a wrong type in any declared field fails the whole payload;
//   - `null` is None for Option fields but an error for `#[serde(default)]
//     Vec` fields;
//   - numbers keep serde_json's integer/float split and its float parsing;
//   - a serde_json::Value field (Muse Code `resets_at`) and the whole OAuth
//     cache (serde flatten buffers it) check every string and number and the
//     nesting limit (serde_json's 128).
// Error texts never quote a value. Known gaps are listed in AGENTS.md.

var errWire = errors.New("unexpected payload")

// recursionLimit is serde_json's remaining_depth: the 128th open container
// on a decoded path is an error.
const recursionLimit = 128

// fields is one decoded JSON object: declared field name to raw value.
type fields map[string]json.RawMessage

// document validates a whole response body as serde_json::from_slice would:
// one JSON value with only whitespace around it. UTF-8 is checked where
// serde reads a string (keys and string fields), not in skipped values.
func document(body []byte) (json.RawMessage, error) {
	if !json.Valid(body) {
		return nil, errWire
	}
	return bytes.TrimSpace(body), nil
}

func isNull(raw json.RawMessage) bool {
	return raw == nil || string(raw) == "null"
}

// object decodes a JSON object, keeping only the declared names. Every key
// must be a valid string (serde parses each one before it skips the value);
// a declared name that appears twice is an error, as serde's "duplicate
// field". raw must be valid JSON.
func object(raw json.RawMessage, declared ...string) (fields, error) {
	b := bytes.TrimSpace(raw)
	if len(b) == 0 || b[0] != '{' {
		return nil, fmt.Errorf("%w: expected an object", errWire)
	}
	out := fields{}
	i := skipSpace(b, 1)
	if b[i] == '}' {
		return out, nil
	}
	for {
		keyEnd := stringEnd(b, i)
		key, err := decodeString(b[i:keyEnd])
		if err != nil {
			return nil, err
		}
		i = skipSpace(b, skipSpace(b, keyEnd)+1)
		valueEnd := valueEnd(b, i)
		if contains(declared, key) {
			if _, dup := out[key]; dup {
				return nil, fmt.Errorf("%w: duplicate field %q", errWire, key)
			}
			out[key] = json.RawMessage(b[i:valueEnd])
		}
		i = skipSpace(b, valueEnd)
		if b[i] == '}' {
			return out, nil
		}
		i = skipSpace(b, i+1)
	}
}

func skipSpace(b []byte, i int) int {
	for i < len(b) && (b[i] == ' ' || b[i] == '\t' || b[i] == '\n' || b[i] == '\r') {
		i++
	}
	return i
}

// stringEnd is the index after the string starting at b[i] (a quote).
func stringEnd(b []byte, i int) int {
	for i++; ; i++ {
		switch b[i] {
		case '\\':
			i++
		case '"':
			return i + 1
		}
	}
}

// valueEnd is the index after the JSON value starting at b[i].
func valueEnd(b []byte, i int) int {
	switch b[i] {
	case '"':
		return stringEnd(b, i)
	case '{', '[':
		depth := 0
		for {
			switch b[i] {
			case '"':
				i = stringEnd(b, i)
				continue
			case '{', '[':
				depth++
			case '}', ']':
				depth--
				if depth == 0 {
					return i + 1
				}
			}
			i++
		}
	}
	for i < len(b) && !bytes.ContainsRune([]byte(",]} \t\r\n"), rune(b[i])) {
		i++
	}
	return i
}

// checkString applies serde_json's checks for a string it reads (not one
// it skips): the bytes are valid UTF-8, and every \u surrogate escape is a
// leading one followed at once by a trailing one. raw is a valid JSON
// string token, quotes included.
func checkString(raw []byte) error {
	body := raw[1 : len(raw)-1]
	if !utf8.Valid(body) {
		return fmt.Errorf("%w: invalid UTF-8 in a string", errWire)
	}
	hex4 := func(i int) (uint64, bool) {
		if i+4 > len(body) {
			return 0, false
		}
		n, err := strconv.ParseUint(string(body[i:i+4]), 16, 16)
		return n, err == nil
	}
	for i := 0; i < len(body); i++ {
		if body[i] != '\\' {
			continue
		}
		i++
		if body[i] != 'u' {
			continue
		}
		n, _ := hex4(i + 1)
		i += 4
		if n >= 0xDC00 && n <= 0xDFFF {
			return fmt.Errorf("%w: lone surrogate escape", errWire)
		}
		if n < 0xD800 || n > 0xDBFF {
			continue
		}
		if i+2 >= len(body) || body[i+1] != '\\' || body[i+2] != 'u' {
			return fmt.Errorf("%w: lone surrogate escape", errWire)
		}
		n2, ok := hex4(i + 3)
		if !ok || n2 < 0xDC00 || n2 > 0xDFFF {
			return fmt.Errorf("%w: lone surrogate escape", errWire)
		}
		i += 6
	}
	return nil
}

// decodeString reads a JSON string token as serde_json does for a String.
func decodeString(raw []byte) (string, error) {
	if len(raw) == 0 || raw[0] != '"' {
		return "", fmt.Errorf("%w: expected a string", errWire)
	}
	if err := checkString(raw); err != nil {
		return "", err
	}
	var s string
	if err := json.Unmarshal(raw, &s); err != nil {
		return "", errWire
	}
	return s, nil
}

// checkValue applies serde_json's checks for a value it reads in full (a
// serde_json::Value, or serde's buffered Content): every string, key, and
// number, and the nesting limit. open is the number of containers already
// open around raw on the decoded path. raw must be valid JSON.
func checkValue(raw []byte, open int) error {
	depth := open
	for i := 0; i < len(raw); {
		switch c := raw[i]; {
		case c == '"':
			end := stringEnd(raw, i)
			if err := checkString(raw[i:end]); err != nil {
				return err
			}
			i = end
		case c == '{' || c == '[':
			depth++
			if depth >= recursionLimit {
				return fmt.Errorf("%w: nesting limit exceeded", errWire)
			}
			i++
		case c == '}' || c == ']':
			depth--
			i++
		case c == '-' || isDigit(c):
			end := valueEnd(raw, i)
			if _, err := number(raw[i:end]); err != nil {
				return fmt.Errorf("%w: number out of range", errWire)
			}
			i = end
		default:
			i++
		}
	}
	return nil
}

func contains(list []string, s string) bool {
	for _, v := range list {
		if v == s {
			return true
		}
	}
	return false
}

// array decodes a JSON array into its raw elements.
func array(raw json.RawMessage) ([]json.RawMessage, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || raw[0] != '[' {
		return nil, fmt.Errorf("%w: expected an array", errWire)
	}
	var out []json.RawMessage
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, errWire
	}
	return out, nil
}

// defaultArray is a `#[serde(default)] Vec<T>` field: absent is empty, null
// is an error.
func (f fields) defaultArray(name string) ([]json.RawMessage, error) {
	raw, ok := f[name]
	if !ok {
		return nil, nil
	}
	return array(raw)
}

// optObject is an `Option<Struct>` field: absent or null is nil.
func (f fields) optObject(name string, declared ...string) (fields, error) {
	raw := f[name]
	if isNull(raw) {
		return nil, nil
	}
	return object(raw, declared...)
}

func (f fields) optString(name string) (*string, error) {
	raw := f[name]
	if isNull(raw) {
		return nil, nil
	}
	s, err := decodeString(bytes.TrimSpace(raw))
	if err != nil {
		return nil, fmt.Errorf("field %q: %w", name, err)
	}
	return &s, nil
}

func (f fields) optBool(name string) (*bool, error) {
	raw := f[name]
	if isNull(raw) {
		return nil, nil
	}
	switch string(bytes.TrimSpace(raw)) {
	case "true":
		v := true
		return &v, nil
	case "false":
		v := false
		return &v, nil
	}
	return nil, fmt.Errorf("%w: field %q: expected a boolean", errWire, name)
}

func (f fields) optFloat(name string) (*float64, error) {
	raw := f[name]
	if isNull(raw) {
		return nil, nil
	}
	n, err := number(raw)
	if err != nil {
		return nil, fmt.Errorf("%w: field %q: expected a number", errWire, name)
	}
	v := n.float()
	return &v, nil
}

func (f fields) optInt64(name string) (*int64, error) {
	raw := f[name]
	if isNull(raw) {
		return nil, nil
	}
	n, err := number(raw)
	if err != nil {
		return nil, fmt.Errorf("%w: field %q: expected an integer", errWire, name)
	}
	switch n.kind {
	case numU64:
		if n.u > math.MaxInt64 {
			return nil, fmt.Errorf("%w: field %q: integer out of range", errWire, name)
		}
		v := int64(n.u)
		return &v, nil
	case numI64:
		v := n.i
		return &v, nil
	}
	return nil, fmt.Errorf("%w: field %q: expected an integer", errWire, name)
}

func (f fields) requiredUint64(name string) (uint64, error) {
	raw, ok := f[name]
	if !ok {
		return 0, fmt.Errorf("missing field `%s`", name)
	}
	n, err := number(raw)
	if err != nil || n.kind != numU64 {
		return 0, fmt.Errorf("field `%s`: expected an unsigned integer", name)
	}
	return n.u, nil
}

func (f fields) requiredString(name string) (string, error) {
	raw, ok := f[name]
	if !ok {
		return "", fmt.Errorf("missing field `%s`", name)
	}
	s, err := decodeString(bytes.TrimSpace(raw))
	if err != nil {
		return "", fmt.Errorf("field `%s`: invalid string", name)
	}
	return s, nil
}

type numKind int

const (
	numU64 numKind = iota
	numI64
	numF64
)

// jsonNumber is a JSON number as serde_json's parser classifies it.
type jsonNumber struct {
	kind numKind
	u    uint64
	i    int64
	f    float64
}

func (n jsonNumber) float() float64 {
	switch n.kind {
	case numU64:
		return float64(n.u)
	case numI64:
		return float64(n.i)
	}
	return n.f
}

// pow10 is serde_json's POW10 table (1e0 through 1e308).
var pow10 = func() [309]float64 {
	var t [309]float64
	for i := range t {
		v, _ := strconv.ParseFloat("1e"+strconv.Itoa(i), 64)
		t[i] = v
	}
	return t
}()

var errNumber = errors.New("not a number")

// number ports serde_json's number parser (default features: no
// float_roundtrip), so floats round exactly as the Rust reader did. raw must
// already be valid JSON.
func number(raw json.RawMessage) (jsonNumber, error) {
	s := bytes.TrimSpace(raw)
	if len(s) == 0 || (s[0] != '-' && (s[0] < '0' || s[0] > '9')) {
		return jsonNumber{}, errNumber
	}
	p := &numParser{s: s}
	positive := true
	if p.peek() == '-' {
		p.pos++
		positive = false
	}
	n, err := p.integer(positive)
	if err != nil {
		return jsonNumber{}, err
	}
	if p.pos != len(s) {
		return jsonNumber{}, errNumber
	}
	return n, nil
}

type numParser struct {
	s   []byte
	pos int
}

func (p *numParser) peek() byte {
	if p.pos < len(p.s) {
		return p.s[p.pos]
	}
	return 0
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

func overflows(a uint64, digit uint64) bool {
	const m = math.MaxUint64
	return a >= m/10 && (a > m/10 || digit > m%10)
}

func (p *numParser) integer(positive bool) (jsonNumber, error) {
	c := p.peek()
	if !isDigit(c) {
		return jsonNumber{}, errNumber
	}
	p.pos++
	if c == '0' {
		if isDigit(p.peek()) {
			return jsonNumber{}, errNumber
		}
		return p.finish(positive, 0)
	}
	sig := uint64(c - '0')
	for isDigit(p.peek()) {
		d := uint64(p.peek() - '0')
		if overflows(sig, d) {
			f, err := p.longInteger(positive, sig)
			return jsonNumber{kind: numF64, f: f}, err
		}
		p.pos++
		sig = sig*10 + d
	}
	return p.finish(positive, sig)
}

func (p *numParser) finish(positive bool, sig uint64) (jsonNumber, error) {
	switch p.peek() {
	case '.':
		f, err := p.decimal(positive, sig, 0)
		return jsonNumber{kind: numF64, f: f}, err
	case 'e', 'E':
		f, err := p.exponent(positive, sig, 0)
		return jsonNumber{kind: numF64, f: f}, err
	}
	if positive {
		return jsonNumber{kind: numU64, u: sig}, nil
	}
	neg := -int64(sig)
	if neg >= 0 {
		return jsonNumber{kind: numF64, f: -float64(sig)}, nil
	}
	return jsonNumber{kind: numI64, i: neg}, nil
}

func (p *numParser) decimal(positive bool, sig uint64, before int32) (float64, error) {
	p.pos++
	var after int32
	for isDigit(p.peek()) {
		d := uint64(p.peek() - '0')
		if overflows(sig, d) {
			for isDigit(p.peek()) {
				p.pos++
			}
			if c := p.peek(); c == 'e' || c == 'E' {
				return p.exponent(positive, sig, before+after)
			}
			return fromParts(positive, sig, before+after)
		}
		p.pos++
		sig = sig*10 + d
		after--
	}
	if after == 0 {
		return 0, errNumber
	}
	if c := p.peek(); c == 'e' || c == 'E' {
		return p.exponent(positive, sig, before+after)
	}
	return fromParts(positive, sig, before+after)
}

func (p *numParser) exponent(positive bool, sig uint64, start int32) (float64, error) {
	p.pos++
	positiveExp := true
	switch p.peek() {
	case '+':
		p.pos++
	case '-':
		p.pos++
		positiveExp = false
	}
	if !isDigit(p.peek()) {
		return 0, errNumber
	}
	exp := int32(p.peek() - '0')
	p.pos++
	for isDigit(p.peek()) {
		d := int32(p.peek() - '0')
		p.pos++
		if exp >= math.MaxInt32/10 && (exp > math.MaxInt32/10 || d > math.MaxInt32%10) {
			if sig != 0 && positiveExp {
				return 0, errNumber
			}
			for isDigit(p.peek()) {
				p.pos++
			}
			if positive {
				return 0, nil
			}
			return math.Copysign(0, -1), nil
		}
		exp = exp*10 + d
	}
	var final int32
	if positiveExp {
		final = satAdd(start, exp)
	} else {
		final = satSub(start, exp)
	}
	return fromParts(positive, sig, final)
}

func (p *numParser) longInteger(positive bool, sig uint64) (float64, error) {
	var exp int32
	for {
		switch c := p.peek(); {
		case isDigit(c):
			p.pos++
			exp++
		case c == '.':
			return p.decimal(positive, sig, exp)
		case c == 'e' || c == 'E':
			return p.exponent(positive, sig, exp)
		default:
			return fromParts(positive, sig, exp)
		}
	}
}

func satAdd(a, b int32) int32 {
	s := int64(a) + int64(b)
	if s > math.MaxInt32 {
		return math.MaxInt32
	}
	if s < math.MinInt32 {
		return math.MinInt32
	}
	return int32(s)
}

func satSub(a, b int32) int32 { return satAdd(a, -b) }

// fromParts is serde_json's f64_from_parts without float_roundtrip.
func fromParts(positive bool, sig uint64, exp int32) (float64, error) {
	f := float64(sig)
	for {
		abs := int64(exp)
		if abs < 0 {
			abs = -abs
		}
		if abs < int64(len(pow10)) {
			if exp >= 0 {
				f *= pow10[abs]
				if math.IsInf(f, 0) {
					return 0, errNumber
				}
			} else {
				f /= pow10[abs]
			}
			break
		}
		if f == 0 {
			break
		}
		if exp >= 0 {
			return 0, errNumber
		}
		f /= 1e308
		exp += 308
	}
	if positive {
		return f, nil
	}
	return -f, nil
}
