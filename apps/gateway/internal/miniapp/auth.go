package miniapp

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"
)

const (
	initDataMaxAgeSecs     = 60 * 60
	initDataFutureSkewSecs = 30
)

type initDataError int

const (
	initDataMissing initDataError = iota + 1
	initDataMalformed
	initDataInvalidSignature
	initDataExpired
	initDataForbidden
)

func (e initDataError) Error() string {
	switch e {
	case initDataMissing:
		return "missing"
	case initDataMalformed:
		return "malformed"
	case initDataInvalidSignature:
		return "invalid signature"
	case initDataExpired:
		return "expired"
	case initDataForbidden:
		return "forbidden"
	}
	return "unknown"
}

// authorize checks `Authorization: tma <initData>` and returns the Telegram
// user id. Statuses and texts match server.rs `authorize`.
func (s *server) authorize(r *http.Request) (int64, *apiError) {
	value, ok := visibleHeader(r.Header.Values("Authorization"))
	if !ok {
		return 0, &apiError{http.StatusUnauthorized, "Telegram authorization required"}
	}
	scheme, initData, found := strings.Cut(value, " ")
	if !found {
		return 0, &apiError{http.StatusUnauthorized, "Invalid Telegram authorization"}
	}
	if !strings.EqualFold(scheme, "tma") || initData == "" {
		return 0, &apiError{http.StatusUnauthorized, "Invalid Telegram authorization"}
	}
	now := s.now().Unix()
	if now < 0 {
		return 0, &apiError{http.StatusInternalServerError, "System clock error"}
	}
	userID, err := validateInitData(initData, s.botToken, s.allowed, uint64(now))
	switch err {
	case nil:
		return userID, nil
	case initDataForbidden:
		return 0, &apiError{http.StatusForbidden, "Telegram user is not allowed"}
	case initDataMissing:
		return 0, &apiError{http.StatusUnauthorized, "Telegram authorization required"}
	default:
		return 0, &apiError{http.StatusUnauthorized, "Invalid or expired Telegram authorization"}
	}
}

// visibleHeader returns the first header value when it is readable text, the
// rule of `HeaderValue::to_str` (visible ASCII, space and tab).
func visibleHeader(values []string) (string, bool) {
	if len(values) == 0 {
		return "", false
	}
	value := values[0]
	for i := 0; i < len(value); i++ {
		c := value[i]
		if c != '\t' && (c < 0x20 || c > 0x7e) {
			return "", false
		}
	}
	return value, true
}

// validateInitData verifies Telegram Mini App `initData`
// (https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app)
// as server.rs `validate_init_data` does: duplicate `hash`, `auth_date` or
// `user` keys are malformed, the age limit is one hour, and at most 30 s of
// clock skew into the future is accepted.
func validateInitData(initData, botToken string, allowed map[int64]struct{}, now uint64) (int64, error) {
	if initData == "" {
		return 0, initDataMissing
	}
	pairs := parseFormPairs(initData)
	var hash, authDate, user *string
	for i := range pairs {
		value := &pairs[i].value
		switch pairs[i].key {
		case "hash":
			if hash != nil {
				return 0, initDataMalformed
			}
			hash = value
		case "auth_date":
			if authDate != nil {
				return 0, initDataMalformed
			}
			authDate = value
		case "user":
			if user != nil {
				return 0, initDataMalformed
			}
			user = value
		}
	}
	if hash == nil {
		return 0, initDataMalformed
	}
	received, ok := decodeHash(*hash)
	if !ok {
		return 0, initDataMalformed
	}
	if authDate == nil {
		return 0, initDataMalformed
	}
	date, ok := parseU64(*authDate)
	if !ok {
		return 0, initDataMalformed
	}
	if date > saturatingAdd(now, initDataFutureSkewSecs) || saturatingSub(now, date) > initDataMaxAgeSecs {
		return 0, initDataExpired
	}

	checked := make([]formPair, 0, len(pairs))
	for _, pair := range pairs {
		if pair.key != "hash" {
			checked = append(checked, pair)
		}
	}
	sort.SliceStable(checked, func(i, j int) bool { return checked[i].key < checked[j].key })
	lines := make([]string, len(checked))
	for i, pair := range checked {
		lines[i] = pair.key + "=" + pair.value
	}
	dataCheck := strings.Join(lines, "\n")

	secret := hmac.New(sha256.New, []byte("WebAppData"))
	secret.Write([]byte(botToken))
	mac := hmac.New(sha256.New, secret.Sum(nil))
	mac.Write([]byte(dataCheck))
	if !hmac.Equal(mac.Sum(nil), received) {
		return 0, initDataInvalidSignature
	}

	if user == nil {
		return 0, initDataMalformed
	}
	id, ok := userID(*user)
	if !ok {
		return 0, initDataMalformed
	}
	if _, ok := allowed[id]; !ok {
		return 0, initDataForbidden
	}
	return id, nil
}

// userID reads the integer `id` of the signed `user` object, as serde
// does for server.rs `InitDataUser`: an exact key match, other keys
// ignored, a repeated `id` refused.
func userID(raw string) (int64, bool) {
	dec := json.NewDecoder(strings.NewReader(raw))
	if open, err := dec.Token(); err != nil || open != json.Delim('{') {
		return 0, false
	}
	var id int64
	seen := false
	for dec.More() {
		key, err := dec.Token()
		if err != nil {
			return 0, false
		}
		if key == "id" {
			if seen || dec.Decode(&id) != nil {
				return 0, false
			}
			seen = true
			continue
		}
		var skip json.RawMessage
		if err := dec.Decode(&skip); err != nil {
			return 0, false
		}
	}
	if _, err := dec.Token(); err != nil {
		return 0, false
	}
	if _, err := dec.Token(); err != io.EOF {
		return 0, false
	}
	return id, seen
}

func decodeHash(hash string) ([]byte, bool) {
	if len(hash) != 64 {
		return nil, false
	}
	bytes, err := hex.DecodeString(hash)
	if err != nil {
		return nil, false
	}
	return bytes, true
}

// parseU64 accepts what Rust `u64::from_str` accepts: digits, one optional
// leading `+`.
func parseU64(value string) (uint64, bool) {
	digits := strings.TrimPrefix(value, "+")
	if digits == "" || digits[0] == '+' || digits[0] == '-' {
		return 0, false
	}
	n, err := strconv.ParseUint(digits, 10, 64)
	return n, err == nil
}

func saturatingAdd(a, b uint64) uint64 {
	if a > ^uint64(0)-b {
		return ^uint64(0)
	}
	return a + b
}

func saturatingSub(a, b uint64) uint64 {
	if b > a {
		return 0
	}
	return a - b
}

type formPair struct{ key, value string }

// parseFormPairs decodes `application/x-www-form-urlencoded` the way the
// Rust `form_urlencoded::parse` does: order and duplicates kept, `+` is a
// space, invalid percent escapes stay literal, invalid UTF-8 is replaced.
// net/url is not used: it drops order, refuses `;`, and fails on bad escapes.
func parseFormPairs(input string) []formPair {
	var pairs []formPair
	for _, part := range strings.Split(input, "&") {
		if part == "" {
			continue
		}
		key, value, _ := strings.Cut(part, "=")
		pairs = append(pairs, formPair{decodeFormComponent(key), decodeFormComponent(value)})
	}
	return pairs
}

func decodeFormComponent(s string) string {
	out := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c == '+':
			out = append(out, ' ')
		case c == '%' && i+2 < len(s) && isHex(s[i+1]) && isHex(s[i+2]):
			out = append(out, unhex(s[i+1])<<4|unhex(s[i+2]))
			i += 2
		default:
			out = append(out, c)
		}
	}
	if utf8.Valid(out) {
		return string(out)
	}
	return strings.ToValidUTF8(string(out), "\uFFFD")
}

func isHex(c byte) bool {
	return ('0' <= c && c <= '9') || ('a' <= c && c <= 'f') || ('A' <= c && c <= 'F')
}

func unhex(c byte) byte {
	switch {
	case '0' <= c && c <= '9':
		return c - '0'
	case 'a' <= c && c <= 'f':
		return c - 'a' + 10
	default:
		return c - 'A' + 10
	}
}
