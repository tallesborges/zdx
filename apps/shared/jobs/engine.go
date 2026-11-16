package jobs

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"syscall"
)

// Engine runs the Rust side of jobs: `zdx jobs attempt` (one attempt's
// agent) and `zdx jobs resolve` (settings). Rust loads config layers from
// the working directory, so Dir is the directory the command ran in.
type Engine struct {
	// Zdx is the Rust binary.
	Zdx  string
	Root string
	// SystemPrompt is the global `--system-prompt`, passed through.
	SystemPrompt *string
	// Dir is the working directory; empty means this process's.
	Dir string
	// Env is the child environment; nil means this process's.
	Env            []string
	Stdout, Stderr io.Writer
}

func (e Engine) args(sub ...string) []string {
	args := []string{"--root", e.Root}
	if e.SystemPrompt != nil {
		args = append(args, "--system-prompt", *e.SystemPrompt)
	}
	return append(append(args, "jobs"), sub...)
}

func (e Engine) command(ctx context.Context, sub ...string) *exec.Cmd {
	cmd := exec.CommandContext(ctx, e.Zdx, e.args(sub...)...)
	cmd.Dir, cmd.Env = e.Dir, e.Env
	return cmd
}

// AttemptInput is the stdin of `zdx jobs attempt`: the declared job values
// and the run's identity.
type AttemptInput struct {
	Job         string  `json:"job"`
	Prompt      string  `json:"prompt"`
	Model       *string `json:"model"`
	Subagent    *string `json:"subagent"`
	Root        *string `json:"root"`
	TimeoutSecs *uint32 `json:"timeout_secs"`
	RunID       string  `json:"run_id"`
	ThreadID    *string `json:"thread_id"`
	NoThread    bool    `json:"no_thread"`
}

// AttemptResult is what `zdx jobs attempt` writes to its result file.
type AttemptResult struct {
	Outcome Outcome `json:"outcome"`
	// Prepared is false when the run could not be prepared (root missing,
	// subagent or model not resolvable); such a run is not retried.
	Prepared bool    `json:"prepared"`
	Error    *string `json:"error"`
	Model    *string `json:"model"`
	Root     *string `json:"root"`
}

// Attempt runs one attempt and waits for it. lock is passed as fd 3 so the
// job stays locked while the attempt lives. Every signal received on
// signals while it runs is sent to it.
func (e Engine) Attempt(in AttemptInput, lock *os.File, signals <-chan os.Signal) AttemptResult {
	fail := func(format string, args ...any) AttemptResult {
		text := summarizeError(fmt.Sprintf(format, args...))
		return AttemptResult{Outcome: Failed, Prepared: true, Error: &text}
	}
	input, err := json.Marshal(in)
	if err != nil {
		return fail("encode attempt input: %v", err)
	}
	resultFile, err := os.CreateTemp("", "zdx-job-attempt-*.json")
	if err != nil {
		return fail("create attempt result file: %v", err)
	}
	resultPath := resultFile.Name()
	resultFile.Close()
	defer os.Remove(resultPath)

	sub := []string{"attempt", "--result", resultPath}
	if lock != nil {
		sub = append(sub, "--lock-fd", "3")
	}
	cmd := e.command(context.Background(), sub...)
	cmd.Stdin = bytes.NewReader(input)
	cmd.Stdout, cmd.Stderr = e.Stdout, e.Stderr
	if lock != nil {
		cmd.ExtraFiles = []*os.File{lock}
	}
	runErr := cmd.Start()
	if runErr == nil {
		done := make(chan struct{})
		go func() {
			for {
				select {
				case sig := <-signals:
					_ = cmd.Process.Signal(sig)
				case <-done:
					return
				}
			}
		}()
		runErr = cmd.Wait()
		close(done)
	}

	if b, err := os.ReadFile(resultPath); err == nil && len(bytes.TrimSpace(b)) > 0 {
		var result AttemptResult
		if err := json.Unmarshal(b, &result); err == nil && validAttemptOutcome(result.Outcome) {
			return result
		}
		return fail("the attempt wrote an unreadable result")
	}
	var exitErr *exec.ExitError
	if errors.As(runErr, &exitErr) {
		if ws, ok := exitErr.Sys().(syscall.WaitStatus); ok && ws.Signaled() {
			text := fmt.Sprintf("the attempt process was killed by %s", ws.Signal())
			return AttemptResult{Outcome: Interrupted, Prepared: true, Error: &text}
		}
		return fail("the attempt process exited (status %d) without a result", exitErr.ExitCode())
	}
	if runErr != nil {
		return fail("could not run the attempt (%s): %v", e.Zdx, runErr)
	}
	return fail("the attempt process exited without a result")
}

func validAttemptOutcome(o Outcome) bool {
	return o == Succeeded || o == Failed || o == Interrupted
}

// ResolveJob is one job's declared values for `zdx jobs resolve`.
type ResolveJob struct {
	Job      string  `json:"job"`
	Model    *string `json:"model"`
	Subagent *string `json:"subagent"`
	Root     *string `json:"root"`
}

// Resolved is one job's resolution: Settings (Rust's JSON, passed through
// as is) or Error.
type Resolved struct {
	Settings json.RawMessage `json:"settings"`
	Error    *string         `json:"error"`
}

// ResolveReply is the output of `zdx jobs resolve`.
type ResolveReply struct {
	// Scheduler is the scheduler context (`--scheduler` only), passed through.
	Scheduler json.RawMessage `json:"scheduler"`
	Results   []Resolved      `json:"results"`
}

// Resolve resolves the settings of jobs, inherited from the caller or (with
// scheduler) from the scheduler's context. prepare also renders each
// run's prompt.
func (e Engine) Resolve(ctx context.Context, jobs []ResolveJob, scheduler, prepare bool) (ResolveReply, error) {
	sub := []string{"resolve"}
	if scheduler {
		sub = append(sub, "--scheduler")
	}
	if prepare {
		sub = append(sub, "--prepare")
	}
	if jobs == nil {
		jobs = []ResolveJob{}
	}
	input, err := json.Marshal(map[string]any{"jobs": jobs})
	if err != nil {
		return ResolveReply{}, err
	}
	cmd := e.command(ctx, sub...)
	cmd.Stdin = bytes.NewReader(input)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return ResolveReply{}, fmt.Errorf("zdx jobs resolve: %v: %s", err, strings.TrimSpace(stderr.String()))
	}
	var reply ResolveReply
	if err := json.Unmarshal(stdout.Bytes(), &reply); err != nil {
		return ResolveReply{}, fmt.Errorf("zdx jobs resolve: unreadable output: %w", err)
	}
	if len(reply.Results) != len(jobs) {
		return ResolveReply{}, fmt.Errorf("zdx jobs resolve: %d results for %d jobs", len(reply.Results), len(jobs))
	}
	if len(reply.Scheduler) == 0 {
		reply.Scheduler = json.RawMessage("null")
	}
	for i := range reply.Results {
		if len(reply.Results[i].Settings) == 0 {
			reply.Results[i].Settings = json.RawMessage("null")
		}
	}
	return reply, nil
}

// errorSummaryMax is the longest error kept in a record, in bytes.
const errorSummaryMax = 400

// summarizeError keeps an error on one line and cuts it to errorSummaryMax
// bytes (at a character boundary) plus `...`.
func summarizeError(text string) string {
	text = strings.ReplaceAll(text, "\n", " | ")
	if len(text) <= errorSummaryMax {
		return text
	}
	end := errorSummaryMax
	for end > 0 && !isCharBoundary(text, end) {
		end--
	}
	return text[:end] + "..."
}

func isCharBoundary(s string, i int) bool {
	return i == len(s) || s[i]&0xC0 != 0x80
}
