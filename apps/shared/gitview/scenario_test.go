package gitview

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fixtureGitEnv makes fixture repositories reproducible: no user or system
// config, a fixed identity, and no terminal prompts. Commit dates are set
// per commit (world.commit), so object ids are the same on every machine.
var fixtureGitEnv = []string{
	"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1", "GIT_TERMINAL_PROMPT=0",
	"GIT_AUTHOR_NAME=Fixture Author", "GIT_AUTHOR_EMAIL=author@example.invalid",
	"GIT_COMMITTER_NAME=Fixture Committer", "GIT_COMMITTER_EMAIL=committer@example.invalid",
}

// fixtureEpoch is the first commit's date; each later commit is an hour on.
var fixtureEpoch = time.Date(2026, 1, 2, 10, 0, 0, 0, time.UTC)

// world is a directory of synthetic repositories, the same for the Rust
// recording and the Go run. dir has its symlinks resolved, as git prints
// it.
type world struct {
	t     *testing.T
	dir   string
	ticks int
	// revs are commit ids the cases refer to as $NAME.
	revs map[string]string
}

func (w *world) path(name string) string { return filepath.Join(w.dir, filepath.FromSlash(name)) }

func (w *world) git(dir string, args ...string) string {
	w.t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = w.path(dir)
	date := fixtureEpoch.Add(time.Duration(w.ticks) * time.Hour).Format(time.RFC3339)
	cmd.Env = append(os.Environ(), append(fixtureGitEnv, "GIT_AUTHOR_DATE="+date, "GIT_COMMITTER_DATE="+date)...)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	out, err := cmd.Output()
	if err != nil {
		w.t.Fatalf("git %v in %s: %v\n%s", args, dir, err, stderr.String())
	}
	return strings.TrimSpace(string(out))
}

func (w *world) write(name string, data string) {
	w.t.Helper()
	p := w.path(name)
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		w.t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(data), 0o644); err != nil {
		w.t.Fatal(err)
	}
}

func (w *world) commit(repo, message string) string {
	w.t.Helper()
	w.git(repo, "add", "-A")
	w.git(repo, "commit", "-q", "-m", message)
	w.ticks++
	return w.git(repo, "rev-parse", "HEAD")
}

func (w *world) init(repo, branch string) {
	w.t.Helper()
	if err := os.MkdirAll(w.path(repo), 0o755); err != nil {
		w.t.Fatal(err)
	}
	w.git(repo, "init", "-q", "-b", branch)
}

// euro is a line of three-byte characters after a prefix of n bytes, so
// the diff limit cuts a character at a different byte for each n.
func euro(n int) string {
	return strings.Repeat("x", n) + strings.Repeat("€", DiffLimitBytes/2) + "\n"
}

// newWorld builds every repository the cases use.
func newWorld(t *testing.T) *world {
	t.Helper()
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	w := &world{t: t, dir: dir, revs: map[string]string{}}

	// changes: every kind of uncommitted change, an upstream it is ahead of
	// and behind, and a second, locked worktree.
	w.init("changes", "main")
	w.write("changes/README.md", "one\n")
	w.write("changes/src/lib.rs", "fn a() {}\n")
	w.write("changes/old name.txt", "rename me\n")
	w.write("changes/gone.txt", "bye\n")
	w.write("changes/binary.bin", "\x00\x01\x02bin\x00")
	w.write("changes/literal*.txt", "initial\n")
	w.write("changes/literal-match.txt", "initial\n")
	w.write("changes/both.txt", "base\n")
	w.commit("changes", "first")
	w.write("changes/README.md", "one\ntwo\n")
	second := w.commit("changes", "second")
	w.git("changes", "remote", "add", "origin", w.path("no-such-remote.git"))
	w.git("changes", "config", "branch.main.remote", "origin")
	w.git("changes", "config", "branch.main.merge", "refs/heads/main")
	remoteOnly := w.git("changes", "commit-tree", second+"^{tree}", "-p", second, "-m", "remote only")
	w.git("changes", "update-ref", "refs/remotes/origin/main", remoteOnly)
	w.write("changes/CHANGELOG.md", "v1\n")
	w.commit("changes", "third, <local> & unpushed")
	w.git("changes", "worktree", "add", "-q", "-b", "side", w.path("changes-side"), second)
	w.git("changes", "worktree", "lock", "--reason", "synthetic", w.path("changes-side"))
	w.write("changes/src/lib.rs", "fn a() {}\nfn b() {}\n")
	w.git("changes", "add", "src/lib.rs")
	w.write("changes/README.md", "one\ntwo\nthree\n")
	w.write("changes/both.txt", "staged\n")
	w.git("changes", "add", "both.txt")
	w.write("changes/both.txt", "staged\nunstaged\n")
	w.git("changes", "mv", "old name.txt", "new name.txt")
	w.git("changes", "rm", "-q", "gone.txt")
	w.write("changes/binary.bin", "\x00\x09\x09bin\x00")
	w.write("changes/literal*.txt", "selected\n")
	w.write("changes/literal-match.txt", "other\n")
	w.write("changes/notes/todo.txt", "todo\n")
	w.write("changes/new file.txt", "fresh <b>&</b>\n")
	w.write("changes/large.txt", strings.Repeat("x", DiffLimitBytes*2))
	w.write("changes/euro-1.txt", euro(1))
	w.write("changes/euro-2.txt", euro(2))
	w.write("changes/euro-3.txt", euro(3))
	w.write("changes/latin1.txt", "caf\xe9 cr\xe8me\n")

	w.init("clean", "main")
	w.write("clean/a.txt", "a\n")
	w.commit("clean", "only")

	// initial: no commit yet.
	w.init("initial", "main")
	w.write("initial/staged.txt", "staged\n")
	w.git("initial", "add", "staged.txt")
	w.write("initial/loose.txt", "loose\n")

	w.init("detached", "main")
	w.write("detached/a.txt", "1\n")
	w.commit("detached", "one")
	w.write("detached/a.txt", "2\n")
	w.commit("detached", "two")
	w.git("detached", "checkout", "-q", "--detach", "HEAD~1")

	// feature: two commits on a branch off main, then local changes.
	w.init("feature", "main")
	w.write("feature/app.txt", "v1\n")
	w.commit("feature", "base")
	w.write("feature/app.txt", "v2\n")
	w.commit("feature", "main two")
	w.git("feature", "checkout", "-q", "-b", "feature")
	w.write("feature/feature.txt", "feature\n")
	w.revs["F1"] = w.commit("feature", "feature one")
	w.write("feature/app.txt", "v3\n")
	w.revs["F2"] = w.commit("feature", "feature two")
	w.write("feature/app.txt", "v4\n")
	w.write("feature/scratch.txt", "scratch\n")

	// origin-head: origin/HEAD names develop, which wins over a local main.
	w.init("origin-head", "develop")
	w.write("origin-head/d.txt", "d\n")
	develop := w.commit("origin-head", "develop")
	w.git("origin-head", "branch", "main")
	w.git("origin-head", "update-ref", "refs/remotes/origin/develop", develop)
	w.git("origin-head", "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/develop")
	w.git("origin-head", "checkout", "-q", "-b", "topic")
	w.write("origin-head/t.txt", "t\n")
	w.commit("origin-head", "topic")

	// remote-only: main exists only as origin/main.
	w.init("remote-only", "work")
	w.write("remote-only/r.txt", "1\n")
	first := w.commit("remote-only", "one")
	w.git("remote-only", "update-ref", "refs/remotes/origin/main", first)
	w.write("remote-only/r.txt", "2\n")
	w.commit("remote-only", "two")

	// trunk: no main branch at all.
	w.init("trunk", "trunk")
	w.write("trunk/t.txt", "1\n")
	w.commit("trunk", "one")
	w.write("trunk/t.txt", "2\n")

	for _, plain := range []string{"plain", "plain-other"} {
		if err := os.MkdirAll(w.path(plain), 0o755); err != nil {
			t.Fatal(err)
		}
		cmd := exec.Command("git", "-C", w.path(plain), "rev-parse", "--show-toplevel")
		if out, err := cmd.Output(); err == nil {
			t.Fatalf("%s is inside a repository (%s): the not-found cases need a directory outside any", plain, out)
		}
	}
	return w
}

// threads are the stored roots of the cases' threads; t-none has no
// thread file.
var threads = map[string]string{
	"t-changes": "changes", "t-sub": "changes/src", "t-clean": "clean", "t-initial": "initial",
	"t-detached": "detached", "t-feature": "feature", "t-origin-head": "origin-head",
	"t-remote-only": "remote-only", "t-trunk": "trunk", "t-plain": "plain",
}

// viewCase is one recorded Rust view: the service's views root (a world
// directory) and the request line, where $NAME is a revision of the world.
type viewCase struct {
	name     string
	fallback string
	view     string
}

func status(name, fallback, thread string) viewCase {
	return viewCase{name, fallback, fmt.Sprintf(`{"op":"git_status","thread_id":%q}`, thread)}
}

func scope(name, thread, scope string) viewCase {
	return viewCase{name, "plain", fmt.Sprintf(`{"op":"git_scope","thread_id":%q,"scope":%q}`, thread, scope)}
}

func diff(name, thread, kind, path string, commit ...string) viewCase {
	view := fmt.Sprintf(`{"op":"git_diff","thread_id":%q,"path":%q,"kind":%q`, thread, path, kind)
	if len(commit) > 0 {
		view += fmt.Sprintf(`,"commit":%q`, commit[0])
	}
	return viewCase{name, "plain", view + "}"}
}

var viewCases = []viewCase{
	status("status/changes", "plain", "t-changes"),
	status("status/subdirectory", "plain", "t-sub"),
	status("status/clean", "plain", "t-clean"),
	status("status/initial", "plain", "t-initial"),
	status("status/detached", "plain", "t-detached"),
	status("status/feature", "plain", "t-feature"),
	status("status/origin-head", "plain", "t-origin-head"),
	status("status/remote-only", "plain", "t-remote-only"),
	status("status/trunk", "plain", "t-trunk"),
	status("status/not-a-repository", "plain", "t-plain"),
	status("status/no-thread-file", "plain", "t-none"),
	status("status/fallback-from-non-repository", "clean", "t-plain"),
	status("status/fallback-without-thread-file", "clean", "t-none"),
	status("status/thread-root-is-fallback", "clean", "t-clean"),

	scope("scope/all-on-main", "t-changes", "all"),
	scope("scope/all-feature", "t-feature", "all"),
	scope("scope/all-trimmed", "t-feature", " all "),
	scope("scope/all-origin-head", "t-origin-head", "all"),
	scope("scope/all-remote-only", "t-remote-only", "all"),
	scope("scope/all-trunk", "t-trunk", "all"),
	scope("scope/all-detached", "t-detached", "all"),
	scope("scope/all-initial", "t-initial", "all"),
	scope("scope/commit", "t-feature", "$F2"),
	scope("scope/commit-short", "t-feature", "$F2:7"),
	scope("scope/commit-unknown", "t-feature", "deadbeefdeadbeef"),
	scope("scope/invalid", "t-feature", "zzz"),
	scope("scope/invalid-revision", "t-feature", "HEAD~1"),
	scope("scope/invalid-without-repository", "t-plain", "zzz"),

	diff("diff/staged", "t-changes", "staged", "src/lib.rs"),
	diff("diff/unstaged", "t-changes", "unstaged", "README.md"),
	diff("diff/both-staged", "t-changes", "staged", "both.txt"),
	diff("diff/both-unstaged", "t-changes", "unstaged", "both.txt"),
	diff("diff/renamed", "t-changes", "staged", "new name.txt"),
	diff("diff/deleted", "t-changes", "staged", "gone.txt"),
	diff("diff/binary", "t-changes", "unstaged", "binary.bin"),
	diff("diff/untracked-nested", "t-changes", "untracked", "notes/todo.txt"),
	diff("diff/untracked-space", "t-changes", "untracked", "new file.txt"),
	diff("diff/untracked-truncated", "t-changes", "untracked", "large.txt"),
	diff("diff/truncated-utf8-1", "t-changes", "untracked", "euro-1.txt"),
	diff("diff/truncated-utf8-2", "t-changes", "untracked", "euro-2.txt"),
	diff("diff/truncated-utf8-3", "t-changes", "untracked", "euro-3.txt"),
	diff("diff/invalid-utf8", "t-changes", "untracked", "latin1.txt"),
	diff("diff/literal-pathspec", "t-changes", "unstaged", "literal*.txt"),
	diff("diff/all-on-main", "t-changes", "all", "README.md"),
	diff("diff/all-untracked-not-in-scope", "t-changes", "all", "new file.txt"),
	diff("diff/all-feature", "t-feature", "all", "app.txt"),
	diff("diff/all-feature-added", "t-feature", "all", "feature.txt"),
	diff("diff/commit", "t-feature", "commit", "feature.txt", "$F1"),
	diff("diff/commit-trimmed", "t-feature", "commit", "feature.txt", " $F1 "),
	diff("diff/commit-not-in-scope", "t-feature", "commit", "app.txt", "$F1"),
	diff("diff/no-longer-staged", "t-changes", "staged", "README.md"),
	diff("diff/no-longer-untracked", "t-changes", "untracked", "README.md"),
	diff("diff/interior-dot-path", "t-changes", "staged", "src/./lib.rs"),
	diff("diff/invalid-parent-path", "t-changes", "staged", "../outside"),
	diff("diff/invalid-absolute-path", "t-changes", "staged", "/etc/hosts"),
	diff("diff/invalid-leading-dot-path", "t-changes", "staged", "./README.md"),
	diff("diff/invalid-kind", "t-changes", "worktree", "README.md"),
	diff("diff/commit-without-hash", "t-feature", "commit", "feature.txt"),
	diff("diff/commit-invalid-hash", "t-feature", "commit", "feature.txt", "HEAD~1"),
	diff("diff/invalid-kind-without-repository", "t-plain", "worktree", "README.md"),
	diff("diff/invalid-path-without-repository", "t-plain", "staged", "../outside"),
}

// expand replaces $NAME and $NAME:N (the first N characters) with the
// world's revisions.
func (w *world) expand(s string) string {
	for name, rev := range w.revs {
		s = strings.ReplaceAll(s, "$"+name+":7", rev[:7])
		s = strings.ReplaceAll(s, "$"+name, rev)
	}
	return s
}
