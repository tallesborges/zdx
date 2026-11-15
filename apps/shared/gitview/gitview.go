// Package gitview builds the Mini App's Git views: the repository of a
// thread (or of the service's views root), its uncommitted state, the files
// a history scope touches, and one file's diff. It is the only
// implementation: the bodies are the JSON the retired Rust Git views
// answered, recorded in testdata/rust_views.json and checked by the golden
// tests (key order aside: the Rust socket body had sorted keys, these
// follow the struct fields).
//
// Which thread a request means, and which stored root that thread has, is
// Rust's: the caller answers it with the `thread_root` view (Locate). This
// package never reads thread files, aliases, or the thread index. It runs the
// `git` executable with bounded output, a deadline per command, and a
// deadline per request.
package gitview

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
)

const (
	// CommandTimeout bounds one git command, as git.rs GIT_COMMAND_TIMEOUT.
	CommandTimeout = 8 * time.Second
	// RequestTimeout bounds one view. It is the socket reply budget the
	// gateway gave the Rust view (socket.ReplyTimeout).
	RequestTimeout = 30 * time.Second
	// DiffLimitBytes caps one diff, as git.rs GIT_DIFF_LIMIT_BYTES.
	DiffLimitBytes = 256 * 1024
	// CommitLimit is how many recent commits a status lists.
	CommitLimit = 24
	// OutputLimitBytes caps the output of every other git command. Rust read
	// it unbounded; a larger output fails the view as Internal.
	OutputLimitBytes = 32 << 20
)

// Error codes, as the Rust ErrorCode names them on the socket.
const (
	CodeInvalid  = "invalid"
	CodeNotFound = "not_found"
	CodeInternal = "internal"
)

// Error is a refused view: the code a route maps to its HTTP status, the
// exact text it answers with, and the cause, for logs only.
type Error struct {
	Code    string
	Message string
	Err     error
}

func (e *Error) Error() string {
	if e.Err == nil {
		return e.Message
	}
	return e.Message + ": " + e.Err.Error()
}

func (e *Error) Unwrap() error { return e.Err }

func refuse(code, message string, cause error) *Error {
	return &Error{Code: code, Message: message, Err: cause}
}

// Selection is the body of the Rust `thread_root` view: the thread a request
// means (`active` resolved; nil when no thread matched), the thread after one
// alias hop, that thread's stored root (nil when it has none), and the
// service's views root, the fallback repository. Both roots are absolute.
type Selection struct {
	ThreadID     *string `json:"thread_id"`
	EffectiveID  *string `json:"effective_id"`
	RootPath     *string `json:"root_path"`
	FallbackRoot string  `json:"fallback_root"`
}

// Locate answers the `thread_root` view for the request's thread. Its
// errors come back from the view unchanged, except an Internal service
// refusal, which becomes Internal "Git repository resolution failed" (the
// text Rust answered when its resolution failed).
type Locate func(ctx context.Context) (Selection, error)

// StatusResponse is the `GET /api/git` body.
type StatusResponse struct {
	Repository Repository `json:"repository"`
	Worktrees  []Worktree `json:"worktrees"`
	Files      Files      `json:"files"`
	Commits    []Commit   `json:"commits"`
}

// Repository names the repository and its branch state. Source is
// `thread` (the thread's stored root) or `bot_root` (the views root).
// ThreadID is the selected thread, before the alias hop.
type Repository struct {
	Name     string  `json:"name"`
	Root     string  `json:"root"`
	Source   string  `json:"source"`
	ThreadID *string `json:"thread_id"`
	Branch   string  `json:"branch"`
	Head     *string `json:"head"`
	Upstream *string `json:"upstream"`
	Ahead    uint64  `json:"ahead"`
	Behind   uint64  `json:"behind"`
	Detached bool    `json:"detached"`
	Clean    bool    `json:"clean"`
}

// Files are the uncommitted changes by category.
type Files struct {
	Staged    []File `json:"staged"`
	Unstaged  []File `json:"unstaged"`
	Untracked []File `json:"untracked"`
}

// File is one changed path with its one-letter status.
type File struct {
	Path         string  `json:"path"`
	Status       string  `json:"status"`
	OriginalPath *string `json:"original_path,omitempty"`
}

// Worktree is one entry of `git worktree list`.
type Worktree struct {
	Path    string   `json:"path"`
	Head    *string  `json:"head"`
	Branch  *string  `json:"branch"`
	Current bool     `json:"current"`
	Flags   []string `json:"flags"`
}

// Commit is one recent commit. RelativeTime is git's `%ar`.
type Commit struct {
	Hash         string `json:"hash"`
	ShortHash    string `json:"short_hash"`
	Author       string `json:"author"`
	AuthoredAt   string `json:"authored_at"`
	RelativeTime string `json:"relative_time"`
	Refs         string `json:"refs"`
	Subject      string `json:"subject"`
}

// ScopeResponse is the `GET /api/git/scope` body.
type ScopeResponse struct {
	// Scope is `all` or `commit`.
	Scope string `json:"scope"`
	// Base is the resolved base revision (`all`) or the commit itself.
	Base *string `json:"base"`
	// BaseRef is the main branch the `all` base came from (`master`,
	// `origin/main`); nil for a commit, or when no main branch was found.
	BaseRef *string `json:"base_ref"`
	// Ahead counts the commits on this branch since the `all` base.
	Ahead int    `json:"ahead"`
	Files []File `json:"files"`
	// Untracked are the untracked paths of Files, which the client asks
	// for with kind=untracked.
	Untracked []string `json:"untracked"`
}

// DiffRequest is one `GET /api/git/diff`. Kind is `staged`, `unstaged`,
// `untracked`, `all`, or `commit` with Commit.
type DiffRequest struct {
	Path   string
	Kind   string
	Commit *string
}

// DiffResponse is the `GET /api/git/diff` body.
type DiffResponse struct {
	Path       string `json:"path"`
	Kind       string `json:"kind"`
	Content    string `json:"content"`
	Bytes      int    `json:"bytes"`
	Lines      int    `json:"lines"`
	Truncated  bool   `json:"truncated"`
	LimitBytes int    `json:"limit_bytes"`
}

// Encode is the JSON body of a response, without HTML escaping (as Rust's
// serde_json writes it) and without a trailing newline.
func Encode(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// located is a resolved repository.
type located struct {
	root     string
	source   string
	threadID *string
}

// resolve picks the repository: the selected thread's stored root, else the
// views root, the first in which `git rev-parse --show-toplevel` answers.
// Rust answers both roots absolute (resolved against the service's
// directory); a relative one would resolve against this process's directory
// instead, so it is never tried.
func resolve(ctx context.Context, locate Locate) (located, error) {
	sel, err := locate(ctx)
	if err != nil {
		var se *protocol.ServiceError
		if errors.As(err, &se) && se.Code == CodeInternal {
			return located{}, refuse(CodeInternal, "Git repository resolution failed", err)
		}
		return located{}, err
	}
	type candidate struct{ source, path string }
	var candidates []candidate
	if sel.RootPath != nil && isAbs(*sel.RootPath) {
		candidates = append(candidates, candidate{"thread", *sel.RootPath})
	}
	if isAbs(sel.FallbackRoot) && (len(candidates) == 0 || !samePath(candidates[0].path, sel.FallbackRoot)) {
		candidates = append(candidates, candidate{"bot_root", sel.FallbackRoot})
	}
	for _, c := range candidates {
		out, err := output(ctx, c.path, "rev-parse", "--show-toplevel")
		if err != nil || !validUTF8(out) {
			continue
		}
		if root := trimLineEnd(string(out)); root != "" {
			return located{root: root, source: c.source, threadID: sel.ThreadID}, nil
		}
	}
	if ctx.Err() != nil {
		// The request ran out of time: not a missing repository.
		return located{}, refuse(CodeInternal, "Git state unavailable", ctx.Err())
	}
	return located{}, refuse(CodeNotFound, "Git repository not found", nil)
}

// Status is the uncommitted state, worktrees, and recent commits of the
// located repository.
func Status(ctx context.Context, locate Locate) (*StatusResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, RequestTimeout)
	defer cancel()
	repo, err := resolve(ctx, locate)
	if err != nil {
		return nil, err
	}
	status, err := loadStatus(ctx, repo.root)
	if err != nil {
		return nil, refuse(CodeInternal, "Git state unavailable", err)
	}

	// Worktrees and commits run at once; the first failure stops the other.
	both, stop := context.WithCancel(ctx)
	defer stop()
	var worktrees []Worktree
	var worktreesErr error
	done := make(chan struct{})
	go func() {
		defer close(done)
		worktrees, worktreesErr = loadWorktrees(both, repo.root)
		if worktreesErr != nil {
			stop()
		}
	}()
	commits := []Commit{}
	var commitsErr error
	if status.oid != nil {
		commits, commitsErr = loadCommits(both, repo.root)
		if commitsErr != nil {
			stop()
		}
	}
	<-done
	if err := errors.Join(commitsErr, worktreesErr); err != nil {
		return nil, refuse(CodeInternal, "Git state unavailable", err)
	}

	for i := range worktrees {
		worktrees[i].Current = samePath(worktrees[i].Path, repo.root)
	}
	detached := status.branch != nil && *status.branch == "(detached)"
	branch := "HEAD"
	switch {
	case detached:
		branch = "detached"
	case status.branch != nil:
		branch = *status.branch
	}
	var head *string
	if status.oid != nil {
		h := shortHash(*status.oid)
		head = &h
	}
	files := status.files
	return &StatusResponse{
		Repository: Repository{
			Name:     repositoryName(repo.root),
			Root:     repo.root,
			Source:   repo.source,
			ThreadID: repo.threadID,
			Branch:   branch,
			Head:     head,
			Upstream: status.upstream,
			Ahead:    status.ahead,
			Behind:   status.behind,
			Detached: detached,
			Clean:    len(files.Staged) == 0 && len(files.Unstaged) == 0 && len(files.Untracked) == 0,
		},
		Worktrees: worktrees,
		Files:     files,
		Commits:   commits,
	}, nil
}

// Scope lists the files a history scope touches: `all` (the merge base
// with the main branch against the working tree, plus untracked files) or a
// commit hash.
func Scope(ctx context.Context, locate Locate, scope string) (*ScopeResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, RequestTimeout)
	defer cancel()
	repo, err := resolve(ctx, locate)
	if err != nil {
		return nil, err
	}
	scope = trimSpace(scope)

	var k kind
	var baseRef *string
	ahead := 0
	switch {
	case scope == "all":
		base := resolveAllBase(ctx, repo.root)
		k, baseRef, ahead = kind{name: "all", rev: base.rev}, base.reference, base.ahead
	case validCommitHash(scope):
		k = kind{name: "commit", rev: scope}
	default:
		return nil, refuse(CodeInvalid, "Invalid Git scope", nil)
	}

	files, err := loadScopeFiles(ctx, repo.root, k)
	if err != nil {
		return nil, refuse(CodeInternal, "Git scope unavailable", err)
	}
	// Untracked files never appear in a diff, so "all changes" would omit
	// brand-new files without them.
	untracked := []File{}
	if k.name == "all" {
		status, err := loadStatus(ctx, repo.root)
		if err != nil {
			return nil, refuse(CodeInternal, "Git state unavailable", err)
		}
		untracked = status.files.Untracked
	}
	files = append(files, untracked...)
	paths := make([]string, 0, len(untracked))
	for _, f := range untracked {
		paths = append(paths, f.Path)
	}
	base := k.rev
	return &ScopeResponse{Scope: k.name, Base: &base, BaseRef: baseRef, Ahead: ahead, Files: files, Untracked: paths}, nil
}

// Diff is one file's diff in a category, bounded to DiffLimitBytes. The
// path is checked first, then the repository, then the category.
func Diff(ctx context.Context, locate Locate, req DiffRequest) (*DiffResponse, error) {
	if !validGitPath(req.Path) {
		return nil, refuse(CodeInvalid, "Invalid Git file path", nil)
	}
	ctx, cancel := context.WithTimeout(ctx, RequestTimeout)
	defer cancel()
	repo, err := resolve(ctx, locate)
	if err != nil {
		return nil, err
	}

	// `all` and `commit` address history, so their base is resolved before
	// the file is checked.
	var k kind
	switch req.Kind {
	case "staged", "unstaged", "untracked":
		k = kind{name: req.Kind}
	case "all":
		k = kind{name: "all", rev: resolveAllBase(ctx, repo.root).rev}
	case "commit":
		hash := ""
		if req.Commit != nil {
			hash = trimSpace(*req.Commit)
		}
		if !validCommitHash(hash) {
			return nil, refuse(CodeInvalid, "Invalid commit hash", nil)
		}
		k = kind{name: "commit", rev: hash}
	default:
		return nil, refuse(CodeInvalid, "Invalid Git file category", nil)
	}

	switch k.name {
	case "staged", "unstaged", "untracked":
		status, err := loadStatus(ctx, repo.root)
		if err != nil {
			return nil, refuse(CodeInternal, "Git state unavailable", err)
		}
		files := map[string][]File{"staged": status.files.Staged, "unstaged": status.files.Unstaged, "untracked": status.files.Untracked}[k.name]
		if !containsPath(files, req.Path) {
			return nil, refuse(CodeNotFound, "Git file is no longer in that category", nil)
		}
	default:
		files, err := loadScopeFiles(ctx, repo.root, k)
		if err != nil {
			return nil, refuse(CodeInternal, "Git scope unavailable", err)
		}
		if !containsPath(files, req.Path) {
			return nil, refuse(CodeNotFound, "Git file is not in that scope", nil)
		}
	}

	raw, truncated, err := loadDiff(ctx, repo.root, req.Path, k)
	if err != nil {
		return nil, refuse(CodeInternal, "Git diff unavailable", err)
	}
	content := lossyUTF8(raw)
	return &DiffResponse{
		Path:       req.Path,
		Kind:       k.name,
		Content:    content,
		Bytes:      len(raw),
		Lines:      countLines(content),
		Truncated:  truncated,
		LimitBytes: DiffLimitBytes,
	}, nil
}

func containsPath(files []File, path string) bool {
	for _, f := range files {
		if f.Path == path {
			return true
		}
	}
	return false
}
