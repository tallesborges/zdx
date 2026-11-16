// Package source fetches what the monitor shows. Views come from the
// engine socket; with CLIFallback they come from `zdx views` while the
// service is down. Subscription quotas are read here directly (shared quota
// package: oauth.json read-only, then each provider over HTTPS), with or
// without the service. Service state and scheduler jobs come from their
// read-only CLI JSON (`zdx service status --json`, `zdx jobs list --scheduler
// --json`). Logs are read from the rolling log files. Nothing here writes,
// starts, or stops anything.
package source

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
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

// Origin says where a view answer came from.
type Origin string

const (
	// OriginSocket: the running engine service answered.
	OriginSocket Origin = "socket"
	// OriginCLI: `zdx views` built the view in its own process.
	OriginCLI Origin = "cli"
	// OriginDirect: the monitor read it itself (subscription quotas).
	OriginDirect Origin = "direct"
)

// CLITimeout bounds one CLI call.
const CLITimeout = 60 * time.Second

// maxCLIOutput bounds what the monitor reads from one CLI call.
const maxCLIOutput = 32 << 20

// Config is what a Source needs.
type Config struct {
	// Root is the project root passed to the CLI (`zdx --root`).
	Root string
	// Home is the ZDX home ($ZDX_HOME) the monitor reads.
	Home string
	// Zdx is the engine binary for CLI calls.
	Zdx string
	// ZdxVersion is an explicit zdx version (--zdx-version) for provider
	// requests; empty means the build-time version (shared/version).
	ZdxVersion string
	// Quota overrides the quota fetch's HTTP client, endpoints, environment,
	// and clock (tests). Home and Version always come from this Config.
	Quota quota.Options
	// CLIFallback lets views run through `zdx views` when the socket has no
	// listener. Off by default: an in-process view refreshes the derived
	// caches (thread index, usage cache) in this process.
	CLIFallback bool
}

// Source is the production backend.
type Source struct {
	cfg    Config
	client *wire.Client
}

// New returns a Source for cfg.
func New(cfg Config) *Source {
	return &Source{cfg: cfg, client: wire.NewClient(SocketPath(cfg.Home))}
}

// SocketPath is the engine socket of a ZDX home.
func SocketPath(home string) string { return wire.SocketPath(home) }

// Config returns the source's settings.
func (s *Source) Config() Config { return s.cfg }

// Status asks the service for its status.
func (s *Source) Status(ctx context.Context) (*wire.StatusReport, error) {
	return s.client.Status(ctx)
}

// View runs one view and returns its body and where it came from.
func (s *Source) View(ctx context.Context, v wire.View) (wire.Output, Origin, error) {
	out, err := s.client.View(ctx, v)
	if err == nil {
		return out, OriginSocket, nil
	}
	if !s.cfg.CLIFallback || !errors.Is(err, wire.ErrNotRunning) {
		return wire.Output{}, OriginSocket, err
	}
	out, err = s.cliView(ctx, v)
	return out, OriginCLI, err
}

func (s *Source) cliView(ctx context.Context, v wire.View) (wire.Output, error) {
	line, err := v.Encode()
	if err != nil {
		return wire.Output{}, err
	}
	stdout, err := s.run(ctx, line, "--root", s.cfg.Root, "views", "--file", "-")
	if err != nil {
		return wire.Output{}, err
	}
	return wire.DecodeViewReply(bytes.TrimSpace(stdout), v.Op)
}

// run runs the engine binary with this home and returns stdout. A non-zero
// exit is an error that carries the last stderr line.
func (s *Source) run(ctx context.Context, stdin []byte, args ...string) ([]byte, error) {
	if s.cfg.Zdx == "" {
		return nil, errors.New("no zdx binary (pass --zdx or put zdx on PATH)")
	}
	ctx, cancel := context.WithTimeout(ctx, CLITimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, s.cfg.Zdx, args...)
	cmd.Env = childEnv(os.Environ(), s.cfg.Home)
	cmd.WaitDelay = 2 * time.Second
	ownGroup(cmd)
	if stdin != nil {
		cmd.Stdin = bytes.NewReader(stdin)
	}
	var stdout, stderr limitedBuffer
	stdout.limit, stderr.limit = maxCLIOutput, 64<<10
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	err := cmd.Run()
	if stdout.over {
		return nil, fmt.Errorf("zdx %s: output over %d bytes", args[0], maxCLIOutput)
	}
	if err != nil {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("zdx %s: %w", strings.Join(args, " "), ctx.Err())
		}
		return nil, fmt.Errorf("zdx %s: %w: %s", strings.Join(args, " "), err, lastLine(stderr.String()))
	}
	return stdout.Bytes(), nil
}

// childEnv is env with ZDX_HOME set to home, so the child reads the same
// home as the monitor.
func childEnv(env []string, home string) []string {
	out := make([]string, 0, len(env)+1)
	for _, kv := range env {
		if !strings.HasPrefix(kv, "ZDX_HOME=") {
			out = append(out, kv)
		}
	}
	return append(out, "ZDX_HOME="+home)
}

func lastLine(s string) string {
	lines := strings.Split(strings.TrimSpace(s), "\n")
	return strings.TrimSpace(lines[len(lines)-1])
}

type limitedBuffer struct {
	bytes.Buffer
	limit int
	over  bool
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	if b.Len()+len(p) > b.limit {
		b.over = true
		return 0, io.ErrShortWrite
	}
	return b.Buffer.Write(p)
}

func decode[T any](body []byte, what string) (T, error) {
	var v T
	if err := json.Unmarshal(body, &v); err != nil {
		return v, fmt.Errorf("decode %s: %w", what, err)
	}
	return v, nil
}

// Services runs `zdx service status --json`: launchd and pidfile state
// without the engine, so a stopped engine still shows.
func (s *Source) Services(ctx context.Context) (data.ServiceStatus, error) {
	out, err := s.run(ctx, nil, "service", "status", "--json")
	if err != nil {
		return data.ServiceStatus{}, err
	}
	return decode[data.ServiceStatus](out, "service status")
}

// Jobs runs `zdx jobs list --scheduler --json`.
func (s *Source) Jobs(ctx context.Context) (data.SchedulerJobs, error) {
	out, err := s.run(ctx, nil, "jobs", "list", "--scheduler", "--json")
	if err != nil {
		return data.SchedulerJobs{}, err
	}
	return decode[data.SchedulerJobs](out, "scheduler jobs")
}

// Snapshot is the `monitor_snapshot` view.
func (s *Source) Snapshot(ctx context.Context) (data.Snapshot, Origin, error) {
	return viewAs[data.Snapshot](ctx, s, wire.View{Op: wire.ViewMonitorSnapshot})
}

// ActiveAgents is the `active_agents` view.
func (s *Source) ActiveAgents(ctx context.Context) ([]data.Agent, Origin, error) {
	return viewAs[[]data.Agent](ctx, s, wire.View{Op: wire.ViewActiveAgents})
}

// Threads is the `threads_list` view (`all` or `workers`).
func (s *Source) Threads(ctx context.Context, kind string) (data.ThreadList, Origin, error) {
	return viewAs[data.ThreadList](ctx, s, wire.View{Op: wire.ViewThreadsList, Kind: &kind})
}

// Thread is the `thread_view` view, with `after` for a delta. SourceID is
// the reply's `source_id`, else the requested id.
func (s *Source) Thread(ctx context.Context, id string, after *uint64) (data.Thread, Origin, error) {
	v := wire.View{Op: wire.ViewThreadView, ThreadID: &id, After: after}
	out, origin, err := s.View(ctx, v)
	if err != nil {
		return data.Thread{}, origin, err
	}
	th, err := decode[data.Thread](out.Body, v.Op)
	th.SourceID = id
	if out.SourceID != nil {
		th.SourceID = *out.SourceID
	}
	return th, origin, err
}

// Trajectory is the `thread_trajectory` view.
func (s *Source) Trajectory(ctx context.Context, id string) (data.Trajectory, Origin, error) {
	return viewAs[data.Trajectory](ctx, s, wire.View{Op: wire.ViewThreadTrajectory, ThreadID: &id})
}

// Usage is the `usage_report` view.
func (s *Source) Usage(ctx context.Context, days uint32) (data.Usage, Origin, error) {
	return viewAs[data.Usage](ctx, s, wire.View{Op: wire.ViewUsageReport, Days: &days})
}

// Quotas fetches every provider account's quota: slow, one network call per
// account. It needs no service. An unknown zdx version, an unreadable
// oauth.json, or a cancelled fetch is an error, never an empty list; a
// failed account is a row with its reason.
func (s *Source) Quotas(ctx context.Context) ([]data.Subscription, Origin, error) {
	v, err := version.Resolve(s.cfg.ZdxVersion)
	if err != nil {
		return nil, OriginDirect, err
	}
	o := s.cfg.Quota
	o.Home, o.Version = s.cfg.Home, v
	snap, err := quota.Fetch(ctx, o)
	if err != nil {
		return nil, OriginDirect, err
	}
	return quota.Subscriptions(snap), OriginDirect, nil
}

// Picker runs `zdx helper` for the catalog model picker of root.
func (s *Source) Picker(ctx context.Context, root string) (data.Picker, error) {
	req, err := json.Marshal(map[string]string{"op": "catalog", "kind": "picker", "root": root, "scope": "general"})
	if err != nil {
		return data.Picker{}, err
	}
	out, err := s.run(ctx, req, "helper", "--file", "-")
	if err != nil {
		return data.Picker{}, err
	}
	reply, err := decode[struct {
		Body data.Picker `json:"body"`
	}](out, "model picker")
	return reply.Body, err
}

// SettingChange is one change of a model setting.
type SettingChange struct {
	Key string
	// Value is the new value; nil resets the key in the target file.
	Value *string
	// Workspace is the workspace root whose overlay is written; empty
	// writes the global config.
	Workspace string
	DryRun    bool
}

// ChangeSetting runs `zdx config set-setting|reset-setting --json`: Rust
// checks the change, writes it, and reports where it applies.
func (s *Source) ChangeSetting(ctx context.Context, ch SettingChange) (data.ChangeOutcome, error) {
	args := []string{"config", "reset-setting", ch.Key}
	if ch.Value != nil {
		args = []string{"config", "set-setting", ch.Key, *ch.Value}
	}
	if ch.Workspace == "" {
		args = append(args, "--global")
	} else {
		args = append(args, "--workspace", ch.Workspace)
	}
	if ch.DryRun {
		args = append(args, "--dry-run")
	}
	out, err := s.run(ctx, nil, append(args, "--json")...)
	if err != nil {
		return data.ChangeOutcome{}, err
	}
	return decode[data.ChangeOutcome](out, "setting change")
}

// Workspaces is the `workspaces` view.
func (s *Source) Workspaces(ctx context.Context) (data.Workspaces, Origin, error) {
	return viewAs[data.Workspaces](ctx, s, wire.View{Op: wire.ViewWorkspaces})
}

func viewAs[T any](ctx context.Context, s *Source, v wire.View) (T, Origin, error) {
	out, origin, err := s.View(ctx, v)
	if err != nil {
		var zero T
		return zero, origin, err
	}
	body, err := decode[T](out.Body, v.Op)
	return body, origin, err
}
