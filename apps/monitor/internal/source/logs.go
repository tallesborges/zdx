package source

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// maxTailBytes bounds one log read: only the end of a large file is read.
const maxTailBytes = 1 << 20

// LogStamp identifies one read of a log file, so an unchanged file is not
// read again.
type LogStamp struct {
	Path  string
	Size  int64
	Mod   time.Time
	Lines int
}

// LogTail is the end of one log file.
type LogTail struct {
	Files     []string // file names, newest first
	File      string   // the file read, "" when none exist
	Stamp     LogStamp
	Lines     []string
	Unchanged bool // same stamp as asked: Lines is empty, keep the old ones
	Truncated bool // the file is longer than what was read
}

// LogDir is the directory of the rolling `zdx.log` files.
func LogDir(home string) string { return filepath.Join(home, "logs") }

// isZdxLog matches the rolling log names; the directory also holds other
// logs (service stdout and stderr, the jobs scheduler).
func isZdxLog(name string) bool { return name == "zdx.log" || strings.HasPrefix(name, "zdx.log.") }

// LogFiles lists `zdx.log*` newest first. The appender names files
// `zdx.log.YYYY-MM-DD`, so a reverse name sort is a reverse date sort.
func LogFiles(home string) ([]string, error) {
	entries, err := os.ReadDir(LogDir(home))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, nil
		}
		return nil, err
	}
	var names []string
	for _, e := range entries {
		if e.Type().IsRegular() && isZdxLog(e.Name()) {
			names = append(names, e.Name())
		}
	}
	sort.Sort(sort.Reverse(sort.StringSlice(names)))
	return names, nil
}

// Log reads the last maxLines lines of the file at index (0 = newest).
// With prev equal to the current stamp it returns Unchanged.
func (s *Source) Log(_ context.Context, index, maxLines int, prev LogStamp) (LogTail, error) {
	return ReadLog(s.cfg.Home, index, maxLines, prev)
}

// ReadLog is Source.Log for a home.
func ReadLog(home string, index, maxLines int, prev LogStamp) (LogTail, error) {
	files, err := LogFiles(home)
	if err != nil {
		return LogTail{}, err
	}
	tail := LogTail{Files: files}
	if len(files) == 0 {
		return tail, nil
	}
	index = min(max(index, 0), len(files)-1)
	tail.File = files[index]
	path := filepath.Join(LogDir(home), tail.File)
	f, err := os.Open(path)
	if err != nil {
		return tail, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return tail, err
	}
	tail.Stamp = LogStamp{Path: path, Size: info.Size(), Mod: info.ModTime(), Lines: maxLines}
	if tail.Stamp == prev {
		tail.Unchanged = true
		return tail, nil
	}
	start := max(info.Size()-maxTailBytes, 0)
	buf := make([]byte, info.Size()-start)
	if _, err := f.ReadAt(buf, start); err != nil && !errors.Is(err, io.EOF) {
		return tail, fmt.Errorf("read %s: %w", path, err)
	}
	if start > 0 {
		// Drop the partial first line.
		if i := bytes.IndexByte(buf, '\n'); i >= 0 {
			buf = buf[i+1:]
		}
		tail.Truncated = true
	}
	lines := strings.Split(strings.TrimRight(string(buf), "\n"), "\n")
	if len(lines) == 1 && lines[0] == "" {
		lines = nil
	}
	if len(lines) > maxLines {
		lines = lines[len(lines)-maxLines:]
		tail.Truncated = true
	}
	tail.Lines = lines
	return tail, nil
}
