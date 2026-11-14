package gitview

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
)

// The parser cases are git.rs's unit tests, ported.

func TestValidCommitHash(t *testing.T) {
	for _, ok := range []string{"723a11e", "8e66e93a1b2c3d4e5f60718293a4b5c6d7e8f900", "ABCDEF0"} {
		if !validCommitHash(ok) {
			t.Errorf("%q refused", ok)
		}
	}
	for _, bad := range []string{"723a11", strings.Repeat("a", 41), "", "--output=/tmp/x", "HEAD~1", "main..HEAD", "723a11e; rm -rf /", "723a11g"} {
		if validCommitHash(bad) {
			t.Errorf("%q accepted", bad)
		}
	}
}

func TestValidGitPath(t *testing.T) {
	for _, ok := range []string{"src/main.rs", "notes/file with spaces.md", "a//b", "a/./b", "a/", ".hidden", "literal*.txt"} {
		if !validGitPath(ok) {
			t.Errorf("%q refused", ok)
		}
	}
	for _, bad := range []string{"", "../outside", "a/../b", "/absolute/path", "./a", ".", ".."} {
		if validGitPath(bad) {
			t.Errorf("%q accepted", bad)
		}
	}
}

func TestSamePathAndName(t *testing.T) {
	if !samePath("/a/b/", "/a/./b") || !samePath("/a//b", "/a/b") || samePath("/a/b", "/a/b/c") || samePath("/a/../b", "/b") {
		t.Fatal("samePath is not Rust's component equality")
	}
	for root, want := range map[string]string{"/w/repo": "repo", "/w/repo/": "repo", "/": "repository", "/w/..": "repository"} {
		if got := repositoryName(root); got != want {
			t.Errorf("repositoryName(%q) = %q, want %q", root, got, want)
		}
	}
}

func TestParseStatus(t *testing.T) {
	out := "# branch.oid 0123456789abcdef\x00" +
		"# branch.head main\x00" +
		"# branch.upstream origin/main\x00" +
		"# branch.ab +2 -1\x00" +
		"1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/lib.rs\x00" +
		"1 M. N... 100644 100644 100644 aaaaaaa bbbbbbb Cargo.toml\x00" +
		"2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 src/new name.rs\x00" +
		"src/old name.rs\x00" +
		"u UU N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc conflict.txt\x00" +
		"? notes/todo.txt\x00"
	s := parseStatus(out)
	if *s.branch != "main" || *s.upstream != "origin/main" || s.ahead != 2 || s.behind != 1 || *s.oid != "0123456789abcdef" {
		t.Fatalf("branch state %+v", s)
	}
	f := s.files
	if f.Unstaged[0].Path != "src/lib.rs" || f.Staged[0].Path != "Cargo.toml" || f.Staged[1].Status != "R" ||
		*f.Staged[1].OriginalPath != "src/old name.rs" || f.Untracked[0].Path != "notes/todo.txt" {
		t.Fatalf("files %+v", f)
	}
	if f.Staged[2].Path != "conflict.txt" || f.Unstaged[1].Path != "conflict.txt" || f.Unstaged[1].Status != "U" {
		t.Fatalf("unmerged entry %+v", f)
	}
	if initial := parseStatus("# branch.oid (initial)\x00# branch.head main\x00"); initial.oid != nil {
		t.Fatal("(initial) is no commit")
	}
	if got := parseCount("+7"); got != 7 {
		t.Fatalf("parseCount(+7) = %d", got)
	}
}

func TestParseWorktreesAndCommits(t *testing.T) {
	worktrees := parseWorktrees("worktree /repo\x00HEAD 0123456789abcdef\x00branch refs/heads/main\x00\x00" +
		"worktree /repo-review\x00HEAD fedcba9876543210\x00detached\x00locked review\x00\x00")
	if len(worktrees) != 2 || *worktrees[0].Branch != "main" || *worktrees[0].Head != "01234567" ||
		strings.Join(worktrees[1].Flags, ",") != "detached,locked" || worktrees[0].Flags == nil {
		t.Fatalf("worktrees %+v", worktrees)
	}
	commits := parseCommits("0123456789abcdef\x1f01234567\x1fAlice\x1f2026-08-24T10:00:00Z\x1f2 hours ago\x1fHEAD -> main\x1fBuild Git view\x1e\n")
	if len(commits) != 1 || commits[0].ShortHash != "01234567" || commits[0].Subject != "Build Git view" {
		t.Fatalf("commits %+v", commits)
	}
	if got := parseCommits(""); got == nil || len(got) != 0 {
		t.Fatal("no commits must encode as []")
	}
}

// lossyUTF8 must replace each maximal invalid subsequence with one U+FFFD,
// as Rust's String::from_utf8_lossy does.
func TestLossyUTF8MatchesRust(t *testing.T) {
	r := "\uFFFD"
	for in, want := range map[string]string{
		"plain":                "plain",
		"a\xE2\x82":            "a" + r,
		"a\xE2\x82b":           "a" + r + "b",
		"\xff\xff":             r + r,
		"\xF0\x9F\x98":         r,
		"\xF0\x9F\x98\x80":     "😀",
		"\xED\xA0\x80":         r + r + r,
		"\xC0\x80":             r + r,
		"\xF4\x90\x80\x80":     r + r + r + r,
		"caf\xe9 cr\xe8me\n":   "caf" + r + " cr" + r + "me\n",
		"\xE0\x80\xAF":         r + r + r,
		"\xE1\x80":             r,
		"\xF1\x80\x80\x41":     r + "A",
		"\xC2":                 r,
		"\xE2\x82\xAC\xE2\x82": "€" + r,
	} {
		if got := lossyUTF8([]byte(in)); got != want {
			t.Errorf("lossyUTF8(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestLinesMatchRust(t *testing.T) {
	for in, want := range map[string]int{"": 0, "\n": 1, "a": 1, "a\n": 1, "a\nb": 2, "a\r\nb\r\n": 2, "\n\n": 2} {
		if got := countLines(in); got != want {
			t.Errorf("countLines(%q) = %d, want %d", in, got, want)
		}
	}
	if got := strings.Join(lines("a\r\nb\r"), "|"); got != "a|b\r" {
		t.Fatalf("lines keeps a \\r that ends no line: %q", got)
	}
}

func TestEncodeKeepsHTMLAndEmptyLists(t *testing.T) {
	got, err := Encode(&ScopeResponse{Scope: "all", Files: []File{{Path: "<a>&b", Status: "A"}}, Untracked: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"scope":"all","base":null,"base_ref":null,"ahead":0,"files":[{"path":"<a>&b","status":"A"}],"untracked":[]}`; string(got) != want {
		t.Fatalf("got %s\nwant %s", got, want)
	}
}

func fixed(sel Selection) Locate {
	return func(context.Context) (Selection, error) { return sel, nil }
}

func refusedWith(t *testing.T, err error, code, message string) {
	t.Helper()
	var ve *Error
	if !errors.As(err, &ve) || ve.Code != code || ve.Message != message {
		t.Fatalf("got %v, want %s %q", err, code, message)
	}
}

func TestLocateErrors(t *testing.T) {
	invalid := &protocol.ServiceError{Code: CodeInvalid, Message: "Invalid thread ID"}
	_, err := Status(context.Background(), func(context.Context) (Selection, error) { return Selection{}, invalid })
	if !errors.Is(err, invalid) {
		t.Fatalf("a refusal of thread_root must pass through: %v", err)
	}
	internal := &protocol.ServiceError{Code: CodeInternal, Message: "Thread root resolution failed"}
	_, err = Scope(context.Background(), func(context.Context) (Selection, error) { return Selection{}, internal }, "all")
	refusedWith(t, err, CodeInternal, "Git repository resolution failed")
	transport := errors.New("socket closed")
	_, err = Diff(context.Background(), func(context.Context) (Selection, error) { return Selection{}, transport }, DiffRequest{Path: "a", Kind: "staged"})
	if !errors.Is(err, transport) {
		t.Fatalf("a transport error must pass through: %v", err)
	}
}

// cleanRepo is a one-commit repository, for tests that need no world.
func cleanRepo(t *testing.T) string {
	t.Helper()
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	w := &world{t: t, dir: dir, revs: map[string]string{}}
	w.init("clean", "main")
	w.write("clean/a.txt", "a\n")
	w.commit("clean", "only")
	return w.path("clean")
}

func TestResolutionEdges(t *testing.T) {
	clean := cleanRepo(t)
	// A relative stored root would resolve against this process's
	// directory, not the service's: it is skipped, and the views root used.
	relative := "clean"
	got, err := Status(context.Background(), fixed(Selection{RootPath: &relative, FallbackRoot: clean}))
	if err != nil || got.Repository.Source != "bot_root" || got.Repository.ThreadID != nil {
		t.Fatalf("relative root: %+v %v", got, err)
	}
	// No candidate at all.
	_, err = Status(context.Background(), fixed(Selection{}))
	refusedWith(t, err, CodeNotFound, "Git repository not found")
	// A request out of time is not a missing repository.
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = Status(ctx, fixed(Selection{FallbackRoot: clean}))
	refusedWith(t, err, CodeInternal, "Git state unavailable")
}

// Without git on PATH every candidate fails, as in Rust: not found.
func TestMissingGit(t *testing.T) {
	clean := cleanRepo(t)
	t.Setenv("PATH", t.TempDir())
	_, err := Status(context.Background(), fixed(Selection{FallbackRoot: clean}))
	refusedWith(t, err, CodeNotFound, "Git repository not found")
}

// Output past the limit kills git and every process it started, and
// returns at once; a deadline does the same.
func TestCaptureBoundsAndCleanup(t *testing.T) {
	if _, err := exec.LookPath("yes"); err != nil {
		t.Fatalf("this test needs yes(1): %v", err)
	}
	root := t.TempDir()
	pidFile := filepath.Join(root, "pid")
	spam := "alias.spam=!echo $$ > '" + pidFile + "'; exec yes"
	start := time.Now()
	out, over, err := capture(context.Background(), root, 10, nil, "-c", spam, "spam")
	if err != nil || !over || len(out) != 10 {
		t.Fatalf("over the limit: %q %v %v", out, over, err)
	}
	if took := time.Since(start); took > 5*time.Second {
		t.Fatalf("capture took %s after the limit", took)
	}
	gone(t, pidFile)
	if _, err := output(context.Background(), root, "-c", spam, "spam"); !errors.Is(err, errOutputLimit) {
		t.Fatalf("output over the limit must fail: %v", err)
	}
	gone(t, pidFile)

	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	start = time.Now()
	_, _, err = capture(ctx, root, 10, nil, "-c", "alias.nap=!echo $$ > '"+pidFile+"'; exec sleep 30", "nap")
	if err == nil || time.Since(start) > 5*time.Second {
		t.Fatalf("deadline: %v after %s", err, time.Since(start))
	}
	gone(t, pidFile)
}

// A helper that leaves git's process group and keeps stdout open does not
// hold the read past the deadline.
func TestCaptureDeadlineWithEscapedHelper(t *testing.T) {
	if _, err := exec.LookPath("perl"); err != nil {
		t.Fatalf("this test needs perl: %v", err)
	}
	root := t.TempDir()
	pidFile := filepath.Join(root, "pid")
	// The child forks, starts its own session, closes stderr, records its
	// pid, and sleeps with git's stdout still open; git itself exits.
	escape := `alias.escape=!perl -MPOSIX -e 'exit if fork; POSIX::setsid(); close STDERR; open my $f, ">", "` + pidFile + `.tmp"; print $f $$; close $f; rename "` + pidFile + `.tmp", "` + pidFile + `"; sleep 60'`
	escaped := func() (int, bool) {
		data, err := os.ReadFile(pidFile)
		if err != nil {
			return 0, false
		}
		pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
		return pid, err == nil
	}
	t.Cleanup(func() {
		if pid, ok := escaped(); ok {
			_ = syscall.Kill(pid, syscall.SIGKILL)
		}
	})
	// The deadline comes only once the helper has left the group, so the
	// kill cannot reach it first.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		defer cancel()
		for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(10 * time.Millisecond) {
			if _, ok := escaped(); ok {
				return
			}
		}
	}()
	start := time.Now()
	_, _, err := capture(ctx, root, 10, nil, "-c", escape, "escape")
	took := time.Since(start)
	if _, ok := escaped(); !ok {
		t.Fatalf("the helper never left git's group: %v", err)
	}
	if err == nil || took > 15*time.Second {
		t.Fatalf("deadline with an escaped helper: %v after %s", err, took)
	}
}

// gone waits until the process whose pid is in pidFile no longer exists.
func gone(t *testing.T, pidFile string) {
	t.Helper()
	data, err := os.ReadFile(pidFile)
	if err != nil {
		t.Fatal(err)
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		t.Fatal(err)
	}
	for deadline := time.Now().Add(3 * time.Second); syscall.Kill(pid, 0) == nil; {
		if time.Now().After(deadline) {
			_ = syscall.Kill(pid, syscall.SIGKILL)
			t.Fatalf("git's child %d outlived it", pid)
		}
		time.Sleep(20 * time.Millisecond)
	}
	os.Remove(pidFile)
}
