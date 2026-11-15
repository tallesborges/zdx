package transcript

import (
	"reflect"
	"strings"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
)

// Clean makes external text safe to lay out: it removes escape
// sequences (SGR, hyperlinks, DCS/APC passthrough), C0 controls except
// newline and tab, DEL, and C1 controls. A carriage return keeps what follows
// it on its line, as a terminal would show it. Thread content, tool output,
// logs, and error strings all come from outside the monitor; the renderer
// would otherwise apply their styles, overwrite cells, or forward
// passthrough sequences to the terminal.
func Clean(s string) string {
	if !needsClean(s) {
		return s
	}
	// A stray byte such as 0x9b would read as a C1 control.
	s = strings.ToValidUTF8(s, "\uFFFD")
	if strings.Contains(s, "\r") {
		lines := strings.Split(strings.ReplaceAll(s, "\r\n", "\n"), "\n")
		for i, l := range lines {
			if j := strings.LastIndexByte(l, '\r'); j >= 0 {
				lines[i] = l[j+1:]
			}
		}
		s = strings.Join(lines, "\n")
	}
	s = ansi.Strip(s)
	return strings.Map(func(r rune) rune {
		switch {
		case r == '\n' || r == '\t':
			return r
		case r < 0x20, r == 0x7f, r >= 0x80 && r <= 0x9f:
			return -1
		}
		return r
	}, s)
}

func needsClean(s string) bool {
	for i := 0; i < len(s); {
		b := s[i]
		if b < utf8.RuneSelf {
			if (b < 0x20 && b != '\n' && b != '\t') || b == 0x7f {
				return true
			}
			i++
			continue
		}
		r, size := utf8.DecodeRuneInString(s[i:])
		if (r == utf8.RuneError && size == 1) || (r >= 0x80 && r <= 0x9f) {
			return true
		}
		i += size
	}
	return false
}

// CleanAll applies Clean to every string reachable from ptr (struct
// fields, pointers, slices, maps). Raw JSON ([]byte) is left alone: its
// strings are still escaped, and the views that show it decode or clean it
// first.
func CleanAll(ptr any) {
	cleanValue(reflect.ValueOf(ptr))
}

func cleanValue(v reflect.Value) {
	switch v.Kind() {
	case reflect.Pointer, reflect.Interface:
		if !v.IsNil() {
			cleanValue(v.Elem())
		}
	case reflect.String:
		if v.CanSet() {
			if s := v.String(); needsClean(s) {
				v.SetString(Clean(s))
			}
		}
	case reflect.Struct:
		for i := 0; i < v.NumField(); i++ {
			if v.Type().Field(i).IsExported() {
				cleanValue(v.Field(i))
			}
		}
	case reflect.Slice, reflect.Array:
		if v.Type().Elem().Kind() == reflect.Uint8 {
			return
		}
		for i := 0; i < v.Len(); i++ {
			cleanValue(v.Index(i))
		}
	case reflect.Map:
		if v.Type().Elem().Kind() != reflect.String {
			return
		}
		for _, k := range v.MapKeys() {
			if s := v.MapIndex(k).String(); needsClean(s) {
				v.SetMapIndex(k, reflect.ValueOf(Clean(s)).Convert(v.Type().Elem()))
			}
		}
	}
}
