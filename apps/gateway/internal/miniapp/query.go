package miniapp

import (
	"net/http"
	"strings"
)

// queryKind is how one query field is read.
type queryKind int

const (
	queryString queryKind = iota
	// queryU64 and queryU32 are Rust `usize`/`u64` and `u32`.
	queryU64
	queryU32
	// queryListKind is server.rs `ThreadListKind`: `all` or `workers`.
	queryListKind
)

type queryField struct {
	name     string
	kind     queryKind
	required bool
}

// queryValues are the fields a request set, by name.
type queryValues map[string]queryValue

type queryValue struct {
	text   string
	number uint64
}

func (v queryValues) text(name string) (string, bool) {
	value, ok := v[name]
	return value.text, ok
}

func (v queryValues) number(name string) (uint64, bool) {
	value, ok := v[name]
	return value.number, ok
}

// parseQuery reads the query string the way axum's `Query<T>` does for the
// server.rs query structs (serde_urlencoded through serde_path_to_error):
// pairs in order, unknown keys ignored, a repeated field is an error, the
// first bad value wins, then the first missing required field in
// declaration order. It answers 400 with axum's text itself on failure.
func parseQuery(w http.ResponseWriter, r *http.Request, fields ...queryField) (queryValues, bool) {
	values := queryValues{}
	for _, pair := range parseFormPairs(r.URL.RawQuery) {
		for _, field := range fields {
			if pair.key != field.name {
				continue
			}
			if _, seen := values[field.name]; seen {
				queryError(w, "duplicate field `"+field.name+"`")
				return nil, false
			}
			value, problem := readQueryValue(field.kind, pair.value)
			if problem != "" {
				queryError(w, field.name+": "+problem)
				return nil, false
			}
			values[field.name] = value
		}
	}
	for _, field := range fields {
		if _, ok := values[field.name]; field.required && !ok {
			queryError(w, "missing field `"+field.name+"`")
			return nil, false
		}
	}
	return values, true
}

func readQueryValue(kind queryKind, text string) (queryValue, string) {
	switch kind {
	case queryU64:
		n, problem := parseRustUint(text, 64)
		return queryValue{number: n}, problem
	case queryU32:
		n, problem := parseRustUint(text, 32)
		return queryValue{number: n}, problem
	case queryListKind:
		if text != "all" && text != "workers" {
			return queryValue{}, "unknown variant `" + text + "`, expected `all` or `workers`"
		}
	}
	return queryValue{text: text}, ""
}

// parseRustUint parses like Rust's `FromStr` for unsigned integers and
// returns its error text: one optional leading `+`, decimal digits only,
// checked digit by digit so the first problem in the string is reported.
func parseRustUint(text string, bits uint) (uint64, string) {
	if text == "" {
		return 0, "cannot parse integer from empty string"
	}
	digits := text
	if text[0] == '+' || text[0] == '-' {
		if len(text) == 1 {
			return 0, "invalid digit found in string"
		}
		if text[0] == '+' {
			digits = text[1:]
		}
	}
	limit := ^uint64(0) >> (64 - bits)
	var n uint64
	for i := 0; i < len(digits); i++ {
		c := digits[i]
		if c < '0' || c > '9' {
			return 0, "invalid digit found in string"
		}
		d := uint64(c - '0')
		if n > (limit-d)/10 {
			return 0, "number too large to fit in target type"
		}
		n = n*10 + d
	}
	return n, ""
}

// queryError answers a rejected query with axum's text.
func queryError(w http.ResponseWriter, detail string) {
	writeText(w, http.StatusBadRequest, "Failed to deserialize query string: "+detail)
}

// blank reports a value that is empty after trimming, as Rust `trim()`.
func blank(value string) bool { return strings.TrimSpace(value) == "" }
