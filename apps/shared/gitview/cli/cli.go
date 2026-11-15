// Package cli is `zdx-go git`, the command line of the Git views that
// `zdx git` execs (crates/zdx-cli/src/cli/commands/git.rs). It prints the
// JSON body the Mini App route answers (package gitview) on stdout.
//
// Usage: zdx-go git [--root ROOT] [--zdx PATH] <command> [--thread ID] ARGS
//
//	status                                      GET /api/git
//	scope [all|COMMIT]                          GET /api/git/scope (default all)
//	diff --kind KIND [--commit COMMIT] PATH     GET /api/git/diff
//
// Without --thread the repository is the one at --root (default: the
// current directory). With --thread, Rust names the thread and its stored
// root through `zdx --root ROOT views` (the `thread_root` view), and --root
// is the fallback when that thread has no stored root. Rust is --zdx, else
// $ZDX_BIN, else `zdx` on PATH.
package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/tallesborges/zdx/apps/shared/gitview"
	"github.com/tallesborges/zdx/apps/shared/protocol"
)

const usage = `usage: zdx-go git [--root ROOT] [--zdx PATH] status [--thread ID]
       zdx-go git [--root ROOT] [--zdx PATH] scope [--thread ID] [all|COMMIT]
       zdx-go git [--root ROOT] [--zdx PATH] diff [--thread ID] --kind KIND [--commit COMMIT] PATH

KIND is staged, unstaged, untracked, all, or commit (with --commit).`

// Main runs `zdx-go git ARGS` and returns the exit code.
func Main(args []string) int {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	return run(ctx, args, os.Stdout, os.Stderr, os.Getenv)
}

type usageError struct{ msg string }

func (e usageError) Error() string { return e.msg }

func flagError(err error) error {
	if errors.Is(err, flag.ErrHelp) {
		return err
	}
	return usageError{err.Error()}
}

func run(ctx context.Context, args []string, stdout, stderr io.Writer, getenv func(string) string) int {
	body, err := view(ctx, args, getenv)
	var u usageError
	switch {
	case errors.Is(err, flag.ErrHelp):
		fmt.Fprintln(stdout, usage)
		return 0
	case errors.As(err, &u):
		fmt.Fprintf(stderr, "zdx-go git: %s\n%s\n", u.msg, usage)
		return 2
	case err != nil:
		fmt.Fprintln(stderr, "zdx-go git:", err)
		return 1
	}
	encoded, err := gitview.Encode(body)
	if err != nil {
		fmt.Fprintln(stderr, "zdx-go git:", err)
		return 1
	}
	if _, err := stdout.Write(append(encoded, '\n')); err != nil {
		return 1
	}
	return 0
}

func view(ctx context.Context, args []string, getenv func(string) string) (any, error) {
	global := flag.NewFlagSet("zdx-go git", flag.ContinueOnError)
	global.SetOutput(io.Discard)
	root := global.String("root", ".", "")
	zdx := global.String("zdx", "", "")
	if err := global.Parse(args); err != nil {
		return nil, flagError(err)
	}
	if global.NArg() == 0 {
		return nil, usageError{"missing command (status, scope, diff)"}
	}
	absRoot, err := filepath.Abs(*root)
	if err != nil {
		return nil, err
	}
	cmd, rest := global.Arg(0), global.Args()[1:]
	fs := flag.NewFlagSet("zdx-go git "+cmd, flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	thread := fs.String("thread", "", "")
	var kind, commit *string
	if cmd == "diff" {
		kind = fs.String("kind", "", "")
		commit = fs.String("commit", "", "")
	}
	if err := fs.Parse(rest); err != nil {
		return nil, flagError(err)
	}
	locate := locator(absRoot, *thread, *zdx, getenv)
	switch cmd {
	case "status":
		if fs.NArg() != 0 {
			return nil, usageError{fmt.Sprintf("unexpected argument %q", fs.Arg(0))}
		}
		return gitview.Status(ctx, locate)
	case "scope":
		scope := "all"
		switch fs.NArg() {
		case 0:
		case 1:
			scope = fs.Arg(0)
		default:
			return nil, usageError{fmt.Sprintf("unexpected argument %q", fs.Arg(1))}
		}
		return gitview.Scope(ctx, locate, scope)
	case "diff":
		if fs.NArg() != 1 {
			return nil, usageError{"diff needs one PATH"}
		}
		if *kind == "" {
			return nil, usageError{"diff needs --kind"}
		}
		req := gitview.DiffRequest{Path: fs.Arg(0), Kind: *kind}
		if *commit != "" {
			req.Commit = commit
		}
		return gitview.Diff(ctx, locate, req)
	}
	return nil, usageError{fmt.Sprintf("unknown command %q", cmd)}
}

// locator is the repository of the request: the directory at root, or,
// for a thread, the `thread_root` view Rust answers with root as the
// fallback.
func locator(root, thread, zdx string, getenv func(string) string) gitview.Locate {
	if thread == "" {
		return func(context.Context) (gitview.Selection, error) {
			return gitview.Selection{FallbackRoot: root}, nil
		}
	}
	return func(ctx context.Context) (gitview.Selection, error) {
		sel, err := threadRoot(ctx, root, thread, zdx, getenv)
		if err != nil {
			return gitview.Selection{}, err
		}
		// The service answers with its own views root as the fallback;
		// this command's root is --root.
		sel.FallbackRoot = root
		return sel, nil
	}
}

// threadRoot runs `zdx --root ROOT views` for the thread's `thread_root`
// view: the running service answers it, or zdx builds it when the service
// is stopped.
func threadRoot(ctx context.Context, root, thread, zdx string, getenv func(string) string) (gitview.Selection, error) {
	bin := zdx
	if bin == "" {
		bin = getenv("ZDX_BIN")
	}
	if bin == "" {
		path, err := exec.LookPath("zdx")
		if err != nil {
			return gitview.Selection{}, fmt.Errorf("find zdx for --thread (pass --zdx or set ZDX_BIN): %w", err)
		}
		bin = path
	}
	line, err := protocol.View{Op: protocol.ViewThreadRoot, ThreadID: &thread}.Encode()
	if err != nil {
		return gitview.Selection{}, err
	}
	cmd := exec.CommandContext(ctx, bin, "--root", root, "views", "--file", "-")
	cmd.Stdin = bytes.NewReader(line)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return gitview.Selection{}, fmt.Errorf("zdx views thread_root: %w: %s", err, lastLine(stderr.String()))
	}
	out, err := protocol.DecodeViewReply(bytes.TrimSpace(stdout.Bytes()), protocol.ViewThreadRoot)
	if err != nil {
		return gitview.Selection{}, err
	}
	var sel gitview.Selection
	if err := json.Unmarshal(out.Body, &sel); err != nil {
		return gitview.Selection{}, fmt.Errorf("decode thread_root body: %w", err)
	}
	return sel, nil
}

func lastLine(s string) string {
	s = strings.TrimSpace(s)
	if i := strings.LastIndexByte(s, '\n'); i >= 0 {
		return s[i+1:]
	}
	return s
}
