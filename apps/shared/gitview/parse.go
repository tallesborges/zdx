package gitview

import (
	"path/filepath"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// parsedStatus is `git status --porcelain=v2 --branch -z`.
type parsedStatus struct {
	oid      *string
	branch   *string
	upstream *string
	ahead    uint64
	behind   uint64
	files    Files
}

func newFiles() Files {
	return Files{Staged: []File{}, Unstaged: []File{}, Untracked: []File{}}
}

func parseStatus(out string) parsedStatus {
	status := parsedStatus{files: newFiles()}
	records := strings.Split(out, "\x00")
	for i := 0; i < len(records); i++ {
		record := records[i]
		switch {
		case strings.HasPrefix(record, "# branch.oid "):
			if oid := strings.TrimPrefix(record, "# branch.oid "); oid != "(initial)" {
				status.oid = &oid
			}
		case strings.HasPrefix(record, "# branch.head "):
			branch := strings.TrimPrefix(record, "# branch.head ")
			status.branch = &branch
		case strings.HasPrefix(record, "# branch.upstream "):
			upstream := strings.TrimPrefix(record, "# branch.upstream ")
			status.upstream = &upstream
		case strings.HasPrefix(record, "# branch.ab "):
			for _, count := range strings.Fields(strings.TrimPrefix(record, "# branch.ab ")) {
				if n, ok := strings.CutPrefix(count, "+"); ok {
					status.ahead = parseCount(n)
				} else if n, ok := strings.CutPrefix(count, "-"); ok {
					status.behind = parseCount(n)
				}
			}
		case strings.HasPrefix(record, "? "):
			status.files.Untracked = append(status.files.Untracked, File{Path: strings.TrimPrefix(record, "? "), Status: "?"})
		case strings.HasPrefix(record, "1 "):
			if xy, path, ok := statusPath(record, 9); ok {
				pushChanged(&status.files, xy, path, nil)
			}
		case strings.HasPrefix(record, "2 "):
			// A rename or copy: the original path is the next record.
			if xy, path, ok := statusPath(record, 10); ok {
				var original *string
				if i+1 < len(records) {
					i++
					original = &records[i]
				}
				pushChanged(&status.files, xy, path, original)
			}
		case strings.HasPrefix(record, "u "):
			if xy, path, ok := statusPath(record, 11); ok {
				pushChanged(&status.files, xy, path, nil)
			}
		}
	}
	return status
}

// parseCount is Rust's `parse::<u64>().unwrap_or_default()`, which also
// takes one leading '+'.
func parseCount(s string) uint64 {
	n, err := strconv.ParseUint(strings.TrimPrefix(s, "+"), 10, 64)
	if err != nil {
		return 0
	}
	return n
}

// statusPath splits a changed-entry record into fields: the XY status is
// the second, the path the last (it may hold spaces).
func statusPath(record string, fields int) (xy, path string, ok bool) {
	parts := strings.SplitN(record, " ", fields)
	if len(parts) != fields {
		return "", "", false
	}
	return parts[1], parts[fields-1], true
}

func pushChanged(files *Files, xy, path string, original *string) {
	r := []rune(xy)
	if len(r) > 0 && r[0] != '.' {
		files.Staged = append(files.Staged, File{Path: path, Status: string(r[0]), OriginalPath: original})
	}
	if len(r) > 1 && r[1] != '.' {
		files.Unstaged = append(files.Unstaged, File{Path: path, Status: string(r[1]), OriginalPath: original})
	}
}

// parseWorktrees is `git worktree list --porcelain -z`. It keeps git.rs's
// builder exactly: a worktree is pushed when the next one starts or at an
// empty field, and fields seen while no path is set carry over to the next
// worktree.
func parseWorktrees(out string) []Worktree {
	worktrees := []Worktree{}
	var path, head, branch *string
	flags := []string{}
	finish := func() {
		if path == nil {
			return
		}
		worktrees = append(worktrees, Worktree{Path: *path, Head: head, Branch: branch, Flags: flags})
		path, head, branch, flags = nil, nil, nil, []string{}
	}
	for _, field := range strings.Split(out, "\x00") {
		switch {
		case field == "":
			finish()
		case strings.HasPrefix(field, "worktree "):
			finish()
			p := strings.TrimPrefix(field, "worktree ")
			path = &p
		case strings.HasPrefix(field, "HEAD "):
			h := shortHash(strings.TrimPrefix(field, "HEAD "))
			head = &h
		case strings.HasPrefix(field, "branch "):
			b := strings.TrimPrefix(strings.TrimPrefix(field, "branch "), "refs/heads/")
			branch = &b
		case field == "detached":
			flags = append(flags, "detached")
		case field == "bare":
			flags = append(flags, "bare")
		case field == "locked" || strings.HasPrefix(field, "locked "):
			flags = append(flags, "locked")
		case field == "prunable" || strings.HasPrefix(field, "prunable "):
			flags = append(flags, "prunable")
		}
	}
	finish()
	return worktrees
}

// parseCommits is the `git log` format of loadCommits: fields split by 0x1f,
// records by 0x1e.
func parseCommits(out string) []Commit {
	commits := []Commit{}
	for _, record := range strings.Split(out, "\x1e") {
		record = strings.Trim(record, "\n\r")
		if record == "" {
			continue
		}
		f := strings.SplitN(record, "\x1f", 7)
		if len(f) != 7 {
			continue
		}
		commits = append(commits, Commit{Hash: f[0], ShortHash: f[1], Author: f[2], AuthoredAt: f[3], RelativeTime: f[4], Refs: f[5], Subject: f[6]})
	}
	return commits
}

// parseNameStatus is `--name-status` output: one `<status>\t<path>` per
// line. Paths that are empty or not repository-relative are dropped.
func parseNameStatus(out string) []File {
	files := []File{}
	for _, line := range lines(out) {
		status, path, ok := strings.Cut(line, "\t")
		if !ok {
			continue
		}
		path = trimSpace(path)
		if path != "" && validGitPath(path) {
			files = append(files, File{Path: path, Status: trimSpace(status)})
		}
	}
	return files
}

// lines is Rust's str::lines: split on \n, drop a \r before each \n, no
// final empty line.
func lines(s string) []string {
	if s == "" {
		return nil
	}
	terminated := strings.HasSuffix(s, "\n")
	parts := strings.Split(strings.TrimSuffix(s, "\n"), "\n")
	for i, p := range parts {
		if i < len(parts)-1 || terminated {
			parts[i] = strings.TrimSuffix(p, "\r")
		}
	}
	return parts
}

// countLines is Rust's `str::lines().count()`.
func countLines(s string) int {
	if s == "" {
		return 0
	}
	n := strings.Count(s, "\n")
	if !strings.HasSuffix(s, "\n") {
		n++
	}
	return n
}

// trimSpace is Rust's str::trim (Unicode White_Space).
func trimSpace(s string) string { return strings.TrimFunc(s, unicode.IsSpace) }

// trimLineEnd is Rust's `trim_end_matches(['\n', '\r'])`.
func trimLineEnd(s string) string { return strings.TrimRight(s, "\n\r") }

// validCommitHash accepts only an abbreviated or full hex object id, so a
// revision that git could read as an option or a range is never built.
func validCommitHash(s string) bool {
	if len(s) < 7 || len(s) > 40 {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		if !('0' <= c && c <= '9' || 'a' <= c && c <= 'f' || 'A' <= c && c <= 'F') {
			return false
		}
	}
	return true
}

// validGitPath is git.rs valid_git_path: a non-empty path whose every
// component (as Rust's Path::components reads it) is a normal name. So no
// root, no `..`, and no leading `.`; empty and interior `.` segments are
// not components and pass.
func validGitPath(path string) bool {
	if path == "" || strings.HasPrefix(path, "/") {
		return false
	}
	for i, segment := range strings.Split(path, "/") {
		switch segment {
		case "..":
			return false
		case ".":
			if i == 0 {
				return false
			}
		}
	}
	return true
}

// components is a path as Rust's Path::components on Unix: a root, then
// the names, without empty or interior `.` segments.
func components(path string) []string {
	var out []string
	if strings.HasPrefix(path, "/") {
		out = append(out, "/")
	}
	for i, segment := range strings.Split(path, "/") {
		switch {
		case segment == "":
		case segment == "." && !(i == 0 && !strings.HasPrefix(path, "/")):
		default:
			out = append(out, segment)
		}
	}
	return out
}

// samePath is Rust's Path equality (component-wise).
func samePath(a, b string) bool {
	ca, cb := components(a), components(b)
	if len(ca) != len(cb) {
		return false
	}
	for i := range ca {
		if ca[i] != cb[i] {
			return false
		}
	}
	return true
}

func isAbs(path string) bool { return path != "" && filepath.IsAbs(path) }

// repositoryName is the last component of the root, as Rust's
// Path::file_name, else "repository".
func repositoryName(root string) string {
	c := components(root)
	if len(c) == 0 {
		return "repository"
	}
	switch last := c[len(c)-1]; last {
	case "/", ".", "..":
		return "repository"
	default:
		return last
	}
}

func shortHash(hash string) string {
	r := []rune(hash)
	if len(r) > 8 {
		r = r[:8]
	}
	return string(r)
}

func validUTF8(b []byte) bool { return utf8.Valid(b) }

// lossyUTF8 is Rust's String::from_utf8_lossy: each maximal invalid
// subsequence becomes one U+FFFD. (Go's own conversions replace either
// every invalid byte or every run of them, which differ from Rust.)
func lossyUTF8(b []byte) string {
	if utf8.Valid(b) {
		return string(b)
	}
	var out strings.Builder
	out.Grow(len(b) + 16)
	cont := func(i int) bool { return i < len(b) && b[i]&0xC0 == 0x80 }
	for i := 0; i < len(b); {
		first := b[i]
		if first < 0x80 {
			out.WriteByte(first)
			i++
			continue
		}
		width, bad := 0, 1
		switch {
		case first >= 0xC2 && first <= 0xDF:
			if cont(i + 1) {
				width = 2
			}
		case first >= 0xE0 && first <= 0xEF:
			if i+1 < len(b) && secondOK(first, b[i+1]) {
				bad = 2
				if cont(i + 2) {
					width = 3
				}
			}
		case first >= 0xF0 && first <= 0xF4:
			if i+1 < len(b) && secondOK(first, b[i+1]) {
				bad = 2
				if cont(i + 2) {
					bad = 3
					if cont(i + 3) {
						width = 4
					}
				}
			}
		}
		if width > 0 {
			out.Write(b[i : i+width])
			i += width
			continue
		}
		out.WriteRune(utf8.RuneError)
		i += bad
	}
	return out.String()
}

// secondOK is the allowed range of the second byte of a 3- or 4-byte
// sequence (no overlongs, no surrogates, nothing past U+10FFFF).
func secondOK(first, second byte) bool {
	switch first {
	case 0xE0:
		return second >= 0xA0 && second <= 0xBF
	case 0xED:
		return second >= 0x80 && second <= 0x9F
	case 0xF0:
		return second >= 0x90 && second <= 0xBF
	case 0xF4:
		return second >= 0x80 && second <= 0x8F
	default:
		return second >= 0x80 && second <= 0xBF
	}
}
