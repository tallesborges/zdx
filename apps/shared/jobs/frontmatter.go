package jobs

import (
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"
)

// splitFrontmatter splits a job file into its frontmatter lines and body.
// Frontmatter starts at the first line (after a BOM) when it is `---` and
// ends at the next `---` or `...` line. Lines end at `\n`; a trailing `\r`
// is dropped.
func splitFrontmatter(content string) (yaml []string, body string, err error) {
	content = strings.TrimPrefix(content, "\ufeff")
	lines := splitLines(content)
	if len(lines) == 0 || strings.TrimSpace(lines[0]) != "---" {
		return nil, "", fmt.Errorf("Missing YAML frontmatter")
	}
	for i := 1; i < len(lines); i++ {
		if t := strings.TrimSpace(lines[i]); t == "---" || t == "..." {
			return lines[1:i], strings.Join(lines[i+1:], "\n"), nil
		}
	}
	return nil, "", fmt.Errorf("Unterminated YAML frontmatter")
}

// splitLines splits like Rust's str::lines: on `\n`, without a final empty
// line, dropping one `\r` before each `\n`.
func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	parts := strings.Split(s, "\n")
	if parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	for i, p := range parts {
		parts[i] = strings.TrimSuffix(p, "\r")
	}
	return parts
}

// scalar is one frontmatter value. A nil *scalar is an unset key.
type scalar struct {
	text   string
	quoted bool
}

var frontmatterKeys = []string{"schedule", "run_at", "grace_secs", "model", "subagent", "root", "timeout_secs", "max_retries"}

// parseFlatYAML reads the frontmatter as a flat mapping of scalars (the
// format docs/SPEC.md §8 Jobs defines). Unset keys are absent from the map.
func parseFlatYAML(lines []string) (map[string]*scalar, error) {
	out := map[string]*scalar{}
	seen := map[string]bool{}
	for i, line := range lines {
		n := i + 2 // the file's line number: line 1 is `---`
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		if line[0] == ' ' || line[0] == '\t' {
			return nil, fmt.Errorf("line %d: nested or indented values are not supported", n)
		}
		key, rest, ok := cutKey(line)
		if !ok {
			return nil, fmt.Errorf("line %d: expected `key: value`", n)
		}
		if !known(key) {
			return nil, fmt.Errorf("unknown field `%s`, expected one of `%s`", key, strings.Join(frontmatterKeys, "`, `"))
		}
		if seen[key] {
			return nil, fmt.Errorf("line %d: duplicate key `%s`", n, key)
		}
		seen[key] = true
		value, err := parseScalar(rest)
		if err != nil {
			return nil, fmt.Errorf("line %d: %s: %w", n, key, err)
		}
		if value != nil {
			out[key] = value
		}
	}
	return out, nil
}

func known(key string) bool {
	for _, k := range frontmatterKeys {
		if k == key {
			return true
		}
	}
	return false
}

// cutKey splits `key: value` (or `key:` at the end of the line).
func cutKey(line string) (key, rest string, ok bool) {
	for i := 0; i < len(line); i++ {
		c := line[i]
		if c == ':' && (i+1 == len(line) || line[i+1] == ' ' || line[i+1] == '\t') {
			key = strings.TrimRight(line[:i], " \t")
			return key, line[i+1:], validKey(key)
		}
		if c == '#' || c == '"' || c == '\'' {
			return "", "", false
		}
	}
	return "", "", false
}

func validKey(key string) bool {
	if key == "" {
		return false
	}
	for _, c := range key {
		if !(c == '_' || c == '-' || c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9') {
			return false
		}
	}
	return true
}

func parseScalar(raw string) (*scalar, error) {
	v := strings.TrimLeft(raw, " \t")
	if v == "" || strings.HasPrefix(v, "#") {
		return nil, nil
	}
	switch v[0] {
	case '"':
		text, rest, err := doubleQuoted(v[1:])
		if err != nil {
			return nil, err
		}
		return &scalar{text: text, quoted: true}, trailing(rest)
	case '\'':
		text, rest, err := singleQuoted(v[1:])
		if err != nil {
			return nil, err
		}
		return &scalar{text: text, quoted: true}, trailing(rest)
	case '[', '{', '&', '*', '!', '|', '>', '%', '@', '`':
		return nil, fmt.Errorf("only plain or quoted scalars are supported")
	case '-', '?':
		if len(v) == 1 || v[1] == ' ' || v[1] == '\t' {
			return nil, fmt.Errorf("only plain or quoted scalars are supported")
		}
	}
	if i := strings.Index(v, " #"); i >= 0 {
		v = v[:i]
	}
	if i := strings.Index(v, "\t#"); i >= 0 {
		v = v[:i]
	}
	v = strings.TrimRight(v, " \t")
	switch v {
	case "~", "null", "Null", "NULL":
		return nil, nil
	}
	if strings.Contains(v, ": ") {
		return nil, fmt.Errorf("only plain or quoted scalars are supported")
	}
	return &scalar{text: v}, nil
}

// trailing accepts only whitespace or a comment after a quoted scalar.
func trailing(rest string) error {
	rest = strings.TrimLeft(rest, " \t")
	if rest == "" || strings.HasPrefix(rest, "#") {
		return nil
	}
	return fmt.Errorf("unexpected text after the quoted value")
}

func singleQuoted(s string) (string, string, error) {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] != '\'' {
			b.WriteByte(s[i])
			continue
		}
		if i+1 < len(s) && s[i+1] == '\'' {
			b.WriteByte('\'')
			i++
			continue
		}
		return b.String(), s[i+1:], nil
	}
	return "", "", fmt.Errorf("unterminated quoted value")
}

func doubleQuoted(s string) (string, string, error) {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch c {
		case '"':
			return b.String(), s[i+1:], nil
		case '\\':
			if i+1 >= len(s) {
				return "", "", fmt.Errorf("unterminated quoted value")
			}
			i++
			switch s[i] {
			case '"', '\\', '/':
				b.WriteByte(s[i])
			case 'n':
				b.WriteByte('\n')
			case 't':
				b.WriteByte('\t')
			case 'r':
				b.WriteByte('\r')
			case '0':
				b.WriteByte(0)
			case 'u':
				if i+4 >= len(s) {
					return "", "", fmt.Errorf("invalid \\u escape")
				}
				r, err := strconv.ParseUint(s[i+1:i+5], 16, 32)
				if err != nil || !utf8.ValidRune(rune(r)) {
					return "", "", fmt.Errorf("invalid \\u escape")
				}
				b.WriteRune(rune(r))
				i += 4
			default:
				return "", "", fmt.Errorf("unsupported escape \\%c", s[i])
			}
		default:
			b.WriteByte(c)
		}
	}
	return "", "", fmt.Errorf("unterminated quoted value")
}
