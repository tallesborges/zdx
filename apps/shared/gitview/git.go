package gitview

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// waitDelay is how long a killed git may keep its output pipes open before
// they are closed under it.
const waitDelay = time.Second

// stderrLimit is how much of a command's stderr is kept for the error.
const stderrLimit = 2048

// errOutputLimit marks a non-diff command whose output passed the limit.
var errOutputLimit = errors.New("Git output exceeded the limit")

// tail keeps the first stderrLimit bytes written and drops the rest, so a
// chatty git never blocks on a full pipe.
type tail struct{ buf bytes.Buffer }

func (t *tail) Write(p []byte) (int, error) {
	if room := stderrLimit - t.buf.Len(); room > 0 {
		t.buf.Write(p[:min(room, len(p))])
	}
	return len(p), nil
}

// capture runs `git -C root args...` with GIT_OPTIONAL_LOCKS=0 (plus env),
// stdin closed, a CommandTimeout deadline, and its own process group, which
// is killed with it. It reads at most limit bytes of stdout: past that it
// kills git and reports over. At the deadline the stdout pipe is closed
// too. The process is always waited for. A non-zero exit is an
// *exec.ExitError.
func capture(ctx context.Context, root string, limit int, env []string, args ...string) (out []byte, over bool, err error) {
	ctx, cancel := context.WithTimeout(ctx, CommandTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", root}, args...)...)
	cmd.Env = append(append(os.Environ(), "GIT_OPTIONAL_LOCKS=0"), env...)
	cmd.WaitDelay = waitDelay
	ownGroup(cmd)
	stderr := &tail{}
	cmd.Stderr = stderr
	pipe, err := cmd.StdoutPipe()
	if err != nil {
		return nil, false, fmt.Errorf("Failed to run Git: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, false, fmt.Errorf("Failed to run Git: %w", err)
	}
	// A helper that left git's process group can keep stdout open after the
	// kill; closing the pipe at the deadline stops the read anyway.
	stopClose := context.AfterFunc(ctx, func() { _ = pipe.Close() })
	defer stopClose()
	out, readErr := io.ReadAll(io.LimitReader(pipe, int64(limit)+1))
	if len(out) > limit {
		cancel()
		_ = cmd.Wait()
		return out[:limit], true, nil
	}
	waitErr := cmd.Wait()
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return nil, false, errors.New("Git command timed out")
	}
	if ctx.Err() != nil {
		return nil, false, ctx.Err()
	}
	if readErr != nil {
		return nil, false, fmt.Errorf("Failed to read Git output: %w", readErr)
	}
	if waitErr != nil {
		var exit *exec.ExitError
		if errors.As(waitErr, &exit) {
			return out, false, fmt.Errorf("Git exited with %w: %s", waitErr, strings.TrimSpace(stderr.buf.String()))
		}
		return nil, false, fmt.Errorf("Failed to run Git: %w", waitErr)
	}
	return out, false, nil
}

// output is git.rs git_output: stdout of a command that must succeed.
func output(ctx context.Context, root string, args ...string) ([]byte, error) {
	out, over, err := capture(ctx, root, OutputLimitBytes, nil, args...)
	if err != nil {
		return nil, err
	}
	if over {
		return nil, fmt.Errorf("%w of %d bytes", errOutputLimit, OutputLimitBytes)
	}
	return out, nil
}

// text is output as UTF-8 text; what names it in the error.
func text(ctx context.Context, root, what string, args ...string) (string, error) {
	out, err := output(ctx, root, args...)
	if err != nil {
		return "", err
	}
	if !validUTF8(out) {
		return "", fmt.Errorf("%s contains non-UTF-8 text", what)
	}
	return string(out), nil
}

func loadStatus(ctx context.Context, root string) (parsedStatus, error) {
	out, err := text(ctx, root, "Git status", "status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all")
	if err != nil {
		return parsedStatus{}, err
	}
	return parseStatus(out), nil
}

func loadWorktrees(ctx context.Context, root string) ([]Worktree, error) {
	out, err := text(ctx, root, "Git worktrees", "worktree", "list", "--porcelain", "-z")
	if err != nil {
		return nil, err
	}
	return parseWorktrees(out), nil
}

func loadCommits(ctx context.Context, root string) ([]Commit, error) {
	out, err := text(ctx, root, "Git log", "log", "-n"+strconv.Itoa(CommitLimit), "--date=iso-strict",
		"--format=%H%x1f%h%x1f%an%x1f%aI%x1f%ar%x1f%D%x1f%s%x1e")
	if err != nil {
		return nil, err
	}
	return parseCommits(out), nil
}

// kind is a diff category. rev is the commit of `commit` and the base of
// `all` (everything since the base, working tree included).
type kind struct {
	name string
	rev  string
}

// loadScopeFiles lists the files of an `all` or `commit` scope.
func loadScopeFiles(ctx context.Context, root string, k kind) ([]File, error) {
	var args []string
	switch k.name {
	case "all":
		args = []string{"diff", "--name-status", "--no-renames", k.rev}
	case "commit":
		args = []string{"show", "--name-status", "--no-renames", "--format=", k.rev}
	default:
		return []File{}, nil
	}
	out, err := text(ctx, root, "Git file list", args...)
	if err != nil {
		return nil, err
	}
	return parseNameStatus(out), nil
}

// allBase is the revision "all changes" diffs against, plus what it came
// from.
type allBase struct {
	// rev is an object id, or HEAD.
	rev string
	// reference is the main branch the base came from; nil when none was
	// found and the scope fell back to HEAD.
	reference *string
	// ahead counts the commits on this branch since the base.
	ahead int
}

// resolveAllBase is the merge base with the repository's main branch, so
// the scope shows what this branch adds, the way a pull request would. On
// the main branch itself the base is HEAD.
func resolveAllBase(ctx context.Context, root string) allBase {
	fallback := allBase{rev: "HEAD"}
	reference, ok := mainBranchRef(ctx, root)
	if !ok {
		return fallback
	}
	out, err := text(ctx, root, "Git merge base", "merge-base", reference, "HEAD")
	if err != nil {
		return fallback
	}
	rev := trimSpace(out)
	if !validCommitHash(rev) {
		return fallback
	}
	ahead := 0
	if count, err := text(ctx, root, "Git count", "rev-list", "--count", rev+"..HEAD"); err == nil {
		if n, err := strconv.ParseUint(trimSpace(count), 10, strconv.IntSize-1); err == nil {
			ahead = int(n)
		}
	}
	return allBase{rev: rev, reference: &reference, ahead: ahead}
}

// mainBranchRef finds the main branch as a revision `git merge-base`
// accepts: the name origin/HEAD points to, then main, then master; a local
// branch first, so unpushed commits on main are not "changes", then the
// origin branch.
func mainBranchRef(ctx context.Context, root string) (string, bool) {
	var candidates []string
	if out, err := text(ctx, root, "Git origin HEAD", "symbolic-ref", "--short", "refs/remotes/origin/HEAD"); err == nil {
		if name, ok := strings.CutPrefix(trimSpace(out), "origin/"); ok {
			candidates = append(candidates, name)
		}
	}
	candidates = append(candidates, "main", "master")
	for _, name := range candidates {
		if refExists(ctx, root, "refs/heads/"+name) {
			return name, true
		}
	}
	for _, name := range candidates {
		if refExists(ctx, root, "refs/remotes/origin/"+name) {
			return "origin/" + name, true
		}
	}
	return "", false
}

func refExists(ctx context.Context, root, ref string) bool {
	_, err := output(ctx, root, "rev-parse", "--verify", "--quiet", ref)
	return err == nil
}

// diffArgs renders one file's diff for a category. The path always follows
// `--`, and GIT_LITERAL_PATHSPECS makes it a literal path.
func diffArgs(k kind, path string) []string {
	flags := []string{"--no-color", "--no-ext-diff", "--no-textconv"}
	switch k.name {
	case "staged":
		return append(append([]string{"diff", "--cached"}, flags...), "--", path)
	case "unstaged":
		return append(append([]string{"diff"}, flags...), "--", path)
	case "untracked":
		return append(append([]string{"diff", "--no-index"}, flags...), "--", "/dev/null", path)
	case "commit":
		return append(append([]string{"show"}, flags...), "--format=", k.rev, "--", path)
	default:
		// The base against the working tree: one diff covers the committed,
		// staged, and unstaged state of the file.
		return append(append([]string{"diff"}, flags...), k.rev, "--", path)
	}
}

// loadDiff reads one diff, at most DiffLimitBytes. `git diff --no-index`
// exits 1 when the files differ, so for untracked files 1 is success.
func loadDiff(ctx context.Context, root, path string, k kind) ([]byte, bool, error) {
	out, over, err := capture(ctx, root, DiffLimitBytes, []string{"GIT_LITERAL_PATHSPECS=1"}, diffArgs(k, path)...)
	if over {
		return out, true, nil
	}
	if err != nil {
		var exit *exec.ExitError
		if k.name == "untracked" && errors.As(err, &exit) && exit.ExitCode() == 1 {
			return out, false, nil
		}
		return nil, false, err
	}
	return out, false, nil
}
