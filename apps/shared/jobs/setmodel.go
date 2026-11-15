package jobs

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode"
)

// SetModel sets (spec non-nil) or clears (nil) the `model` key of the job
// file at path and returns the updated definition.
//
// Only the `model:` frontmatter line changes (an inline comment on it is
// kept); every other key, comment, and the prompt stay byte for byte. A new
// key is added as the last frontmatter line. The result is parsed before it
// is written and must differ from the original only in `model`. The file is
// re-read just before the atomic rename, and the write is abandoned if it
// changed in between.
func SetModel(path string, spec *string) (Definition, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return Definition{}, fmt.Errorf("read job file %s: %w", path, err)
	}
	original := string(b)
	before, err := ParseContent(path, original)
	if err != nil {
		return Definition{}, fmt.Errorf("parse job %s: %w", path, err)
	}
	if spec != nil {
		v, err := validateModelSpec(*spec)
		if err != nil {
			return Definition{}, err
		}
		spec = &v
	}
	if equalPtr(before.Model, spec) {
		return before, nil
	}
	updated, err := editModelLine(original, spec)
	if err != nil {
		return Definition{}, err
	}
	after, err := ParseContent(path, updated)
	if err != nil {
		return Definition{}, fmt.Errorf("the edited job no longer parses; file left unchanged: %w", err)
	}
	if !equalPtr(after.Model, spec) || !sameExceptModel(before, after) {
		return Definition{}, fmt.Errorf("cannot edit the model of '%s' safely; file left unchanged", before.Name)
	}
	if err := writeIfUnchanged(path, original, updated); err != nil {
		return Definition{}, err
	}
	return after, nil
}

func validateModelSpec(spec string) (string, error) {
	spec = strings.TrimSpace(spec)
	if spec == "" {
		return "", fmt.Errorf("model cannot be empty")
	}
	for _, c := range spec {
		if c == '"' || c == '\\' || unicode.IsControl(c) || unicode.IsSpace(c) {
			return "", fmt.Errorf("invalid model spec '%s'", spec)
		}
	}
	return spec, nil
}

// editModelLine replaces, removes, or appends the top-level `model:` line.
func editModelLine(content string, spec *string) (string, error) {
	lines := strings.SplitAfter(content, "\n")
	if lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	closing := -1
	for i := 1; i < len(lines); i++ {
		if t := strings.TrimSpace(lines[i]); t == "---" || t == "..." {
			closing = i
			break
		}
	}
	if closing < 0 {
		return "", fmt.Errorf("Unterminated YAML frontmatter")
	}
	var modelLines []int
	for i := 1; i < closing; i++ {
		if isModelKeyLine(lines[i]) {
			modelLines = append(modelLines, i)
		}
	}
	if len(modelLines) > 1 {
		return "", fmt.Errorf("frontmatter has more than one `model` key")
	}
	newline := "\n"
	if strings.HasSuffix(lines[0], "\r\n") {
		newline = "\r\n"
	}
	out := append([]string{}, lines...)
	switch {
	case len(modelLines) == 1 && spec != nil:
		line := lines[modelLines[0]]
		body := strings.TrimRight(line, "\r\n")
		ending := line[len(body):]
		out[modelLines[0]] = fmt.Sprintf("model: \"%s\"%s%s", *spec, inlineComment(body), ending)
	case len(modelLines) == 1:
		out = append(out[:modelLines[0]], out[modelLines[0]+1:]...)
	case spec != nil:
		out = append(out[:closing], append([]string{fmt.Sprintf("model: \"%s\"%s", *spec, newline)}, out[closing:]...)...)
	}
	return strings.Join(out, ""), nil
}

func isModelKeyLine(line string) bool {
	rest, ok := strings.CutPrefix(line, "model")
	return ok && strings.HasPrefix(strings.TrimLeft(rest, " \t"), ":")
}

// inlineComment is the ` # comment` tail of a `model: value` line, or "".
func inlineComment(line string) string {
	colon := strings.Index(line, ":")
	if colon < 0 {
		colon = len(line)
	} else {
		colon++
	}
	value := strings.TrimLeft(line[colon:], " \t\n\r\v\f")
	offset := len(line) - len(value)
	searchFrom := 0
	if value != "" && (value[0] == '"' || value[0] == '\'') {
		if end := strings.IndexByte(value[1:], value[0]); end >= 0 {
			searchFrom = end + 2
		} else {
			searchFrom = len(value)
		}
	}
	i := strings.Index(value[searchFrom:], " #")
	if i < 0 {
		return ""
	}
	return strings.TrimRight(line[offset+searchFrom+i:], " \t\n\r\v\f")
}

func writeIfUnchanged(path, original, updated string) error {
	tmp := filepath.Join(filepath.Dir(path), fmt.Sprintf(".%s.%d.tmp", filepath.Base(path), os.Getpid()))
	if err := os.WriteFile(tmp, []byte(updated), 0o666); err != nil {
		return fmt.Errorf("write %s: %w", tmp, err)
	}
	err := func() error {
		if info, err := os.Stat(path); err == nil {
			if err := os.Chmod(tmp, info.Mode().Perm()); err != nil {
				return fmt.Errorf("copy permissions to %s: %w", tmp, err)
			}
		}
		current, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("re-read job file %s: %w", path, err)
		}
		if string(current) != original {
			return fmt.Errorf("%s changed on disk while saving; reload and try again", path)
		}
		if err := os.Rename(tmp, path); err != nil {
			return fmt.Errorf("replace %s: %w", path, err)
		}
		return nil
	}()
	if err != nil {
		_ = os.Remove(tmp)
	}
	return err
}

func sameExceptModel(a, b Definition) bool {
	return a.Name == b.Name && a.Path == b.Path && a.Schedule.Kind == b.Schedule.Kind &&
		a.Schedule.Cron == b.Schedule.Cron && a.Schedule.RunAt.Equal(b.Schedule.RunAt) &&
		a.Schedule.RunAt.Format(offsetLayout) == b.Schedule.RunAt.Format(offsetLayout) &&
		equalPtr(a.GraceSecs, b.GraceSecs) && equalPtr(a.Subagent, b.Subagent) && equalPtr(a.Root, b.Root) &&
		equalPtr(a.TimeoutSecs, b.TimeoutSecs) && a.MaxRetries == b.MaxRetries && a.Prompt == b.Prompt
}

const offsetLayout = "2006-01-02T15:04:05-07:00"

func equalPtr[T comparable](a, b *T) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}
