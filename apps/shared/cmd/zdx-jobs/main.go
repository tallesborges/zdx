// Command zdx-jobs is the jobs program: every `zdx jobs` subcommand except
// `history` (and the internal `attempt` and `resolve`) execs it
// (crates/zdx-cli/src/cli/commands/go_program.rs). It owns job files, the
// scheduler, and run bookkeeping (package jobs); it runs Rust `zdx` for
// each attempt's agent and for settings resolution.
//
// Usage: zdx-jobs [--root ROOT] [--system-prompt TEXT] [--no-thread]
// [--thread ID] [--zdx PATH] <list|validate|daemon|runs|set-model|run> ARGS
//
// The ZDX home is $ZDX_HOME, else ~/.zdx. Rust is --zdx, else $ZDX_BIN,
// else `zdx` on PATH.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/tallesborges/zdx/apps/shared/jobs"
)

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

// globals are the flags before the subcommand, the global flags of `zdx`
// that jobs use.
type globals struct {
	root         string
	systemPrompt *string
	noThread     bool
	thread       *string
	zdx          string
}

// childArgs are the global flags a scheduled run child gets.
func (g globals) childArgs() []string {
	args := []string{"--root", g.root}
	if g.systemPrompt != nil {
		args = append(args, "--system-prompt", *g.systemPrompt)
	}
	if g.noThread {
		args = append(args, "--no-thread")
	}
	return args
}

type app struct {
	g              globals
	home           jobs.Home
	stdout, stderr io.Writer
	log            *slog.Logger
}

func run(args []string, stdout, stderr io.Writer) int {
	g, rest, err := parseGlobals(args)
	if err != nil {
		fmt.Fprintln(stderr, "zdx-jobs:", err)
		return 2
	}
	if len(rest) == 0 {
		fmt.Fprintln(stderr, "zdx-jobs: missing subcommand (list, validate, daemon, runs, set-model, run)")
		return 2
	}
	home, err := zdxHome()
	if err != nil {
		fmt.Fprintln(stderr, "zdx-jobs:", err)
		return 1
	}
	a := &app{g: g, home: jobs.Home{Dir: home}, stdout: stdout, stderr: stderr,
		log: slog.New(slog.NewTextHandler(stderr, nil))}
	sub, subArgs := rest[0], rest[1:]
	commands := map[string]func([]string) error{
		"list": a.list, "validate": a.validate, "daemon": a.daemon, "runs": a.runs,
		"set-model": a.setModel, "run": a.runJob,
	}
	cmd, ok := commands[sub]
	if !ok {
		fmt.Fprintf(stderr, "zdx-jobs: unknown subcommand %q\n", sub)
		return 2
	}
	err = cmd(subArgs)
	var usage usageError
	var busy jobs.ErrBusy
	switch {
	case err == nil:
		return 0
	case errors.As(err, &usage):
		fmt.Fprintln(stderr, "zdx-jobs:", err)
		return 2
	case errors.As(err, &busy):
		fmt.Fprintln(stderr, err)
		return jobs.ExitBusy
	case errors.Is(err, jobs.ErrInterrupted):
		fmt.Fprintln(stderr, err)
		return 130
	}
	fmt.Fprintln(stderr, err)
	return 1
}

func zdxHome() (string, error) {
	if h := os.Getenv("ZDX_HOME"); h != "" {
		return h, nil
	}
	dir, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("cannot find the home directory: %w", err)
	}
	return filepath.Join(dir, ".zdx"), nil
}

type usageError struct{ msg string }

func (e usageError) Error() string { return e.msg }

func usagef(format string, args ...any) error { return usageError{fmt.Sprintf(format, args...)} }

func parseGlobals(args []string) (globals, []string, error) {
	var g globals
	for len(args) > 0 && strings.HasPrefix(args[0], "--") {
		name, value, hasValue := strings.Cut(args[0], "=")
		takeValue := func() (string, error) {
			if hasValue {
				args = args[1:]
				return value, nil
			}
			if len(args) < 2 {
				return "", usagef("%s needs a value", name)
			}
			v := args[1]
			args = args[2:]
			return v, nil
		}
		var err error
		switch name {
		case "--root":
			g.root, err = takeValue()
		case "--system-prompt":
			var v string
			v, err = takeValue()
			g.systemPrompt = &v
		case "--thread":
			var v string
			v, err = takeValue()
			g.thread = &v
		case "--zdx":
			g.zdx, err = takeValue()
		case "--no-thread":
			g.noThread = true
			args = args[1:]
		default:
			return g, nil, usagef("unknown flag %s", name)
		}
		if err != nil {
			return g, nil, err
		}
	}
	if g.root == "" {
		wd, err := os.Getwd()
		if err != nil {
			return g, nil, err
		}
		g.root = wd
	}
	return g, args, nil
}

// flags parses subcommand args: bool flags, value flags, and positionals in
// any order.
type flags struct {
	bools  map[string]bool
	values map[string]*string
	pos    []string
}

func parseFlags(args []string, boolNames, valueNames []string, maxPos int) (flags, error) {
	f := flags{bools: map[string]bool{}, values: map[string]*string{}}
	isBool := map[string]bool{}
	for _, n := range boolNames {
		isBool[n] = true
	}
	isValue := map[string]bool{}
	for _, n := range valueNames {
		isValue[n] = true
	}
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if !strings.HasPrefix(arg, "--") || arg == "--" {
			f.pos = append(f.pos, arg)
			continue
		}
		name, value, hasValue := strings.Cut(arg[2:], "=")
		switch {
		case isBool[name] && !hasValue:
			f.bools[name] = true
		case isValue[name] && hasValue:
			f.values[name] = &value
		case isValue[name] && i+1 < len(args):
			i++
			v := args[i]
			f.values[name] = &v
		case isValue[name]:
			return f, usagef("--%s needs a value", name)
		default:
			return f, usagef("unknown flag %s", arg)
		}
	}
	if len(f.pos) > maxPos {
		return f, usagef("unexpected argument %q", f.pos[maxPos])
	}
	return f, nil
}

func (a *app) engine() jobs.Engine {
	zdx := a.g.zdx
	if zdx == "" {
		zdx = os.Getenv("ZDX_BIN")
	}
	if zdx == "" {
		if path, err := exec.LookPath("zdx"); err == nil {
			a.log.Warn("ZDX_BIN is not set; running zdx from PATH", "zdx", path)
			zdx = path
		} else {
			zdx = "zdx"
		}
	}
	return jobs.Engine{Zdx: zdx, Root: a.g.root, SystemPrompt: a.g.systemPrompt, Stdout: a.stdout, Stderr: a.stderr}
}

// printJSON prints v indented by two spaces, without HTML escaping, as
// serde_json's pretty printer did.
func printJSON(w io.Writer, v any) error {
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}

// ---- list ----

// overview is the listing view of one job (`zdx jobs list --json`).
type overview struct {
	Name     string         `json:"name"`
	Path     string         `json:"path"`
	Schedule *jobs.Schedule `json:"schedule"`
	// GraceSecs is the effective grace; nil for manual and invalid jobs.
	GraceSecs *uint32 `json:"grace_secs"`
	jobs.State
	DeclaredRoot  *string         `json:"declared_root"`
	DeclaredModel *string         `json:"declared_model"`
	Subagent      *string         `json:"subagent"`
	TimeoutSecs   *uint32         `json:"timeout_secs"`
	MaxRetries    uint32          `json:"max_retries"`
	Settings      json.RawMessage `json:"settings"`
	Error         *string         `json:"error"`
}

// settingsView is the part of Rust's settings the text output shows.
type settingsView struct {
	Root struct {
		Path   string `json:"path"`
		Origin string `json:"origin"`
	} `json:"root"`
	Model struct {
		Spec   string `json:"spec"`
		Origin string `json:"origin"`
	} `json:"model"`
}

func (o overview) settings() (settingsView, bool) {
	var s settingsView
	if len(o.Settings) == 0 || string(o.Settings) == "null" {
		return s, false
	}
	return s, json.Unmarshal(o.Settings, &s) == nil
}

func resolveJobs(cat *jobs.Catalog) []jobs.ResolveJob {
	out := make([]jobs.ResolveJob, len(cat.Jobs))
	for i, j := range cat.Jobs {
		out[i] = jobs.ResolveJob{Job: j.Name, Model: j.Model, Subagent: j.Subagent, Root: j.Root}
	}
	return out
}

// overviews builds the listing views: valid jobs with their resolution,
// then invalid files, sorted by name. failed is the error every job gets
// when resolution could not run at all.
func (a *app) overviews(cat *jobs.Catalog, results []jobs.Resolved, failed string) []overview {
	now := time.Now()
	out := make([]overview, 0, len(cat.Jobs)+len(cat.Invalid))
	for i := range cat.Jobs {
		j := &cat.Jobs[i]
		schedule := j.Schedule
		o := overview{Name: j.Name, Path: j.Path, Schedule: &schedule, State: jobs.StateOf(a.home, j, now, time.Local),
			DeclaredRoot: j.Root, DeclaredModel: j.Model, Subagent: j.Subagent, TimeoutSecs: j.TimeoutSecs,
			MaxRetries: j.MaxRetries, Settings: json.RawMessage("null")}
		if g, ok := j.EffectiveGraceSecs(); ok {
			o.GraceSecs = &g
		}
		switch {
		case results != nil:
			o.Settings, o.Error = results[i].Settings, results[i].Error
		default:
			text := failed
			o.Error = &text
		}
		out = append(out, o)
	}
	for _, inv := range cat.Invalid {
		text := inv.Error
		out = append(out, overview{Name: inv.Name, Path: inv.Path, State: jobs.State{Status: jobs.StatusInvalid},
			Settings: json.RawMessage("null"), Error: &text})
	}
	sortOverviews(out)
	return out
}

func sortOverviews(out []overview) {
	sort.SliceStable(out, func(i, j int) bool { return out[i].Name < out[j].Name })
}

func (a *app) list(args []string) error {
	f, err := parseFlags(args, []string{"json", "daemon", "archived"}, nil, 0)
	if err != nil {
		return err
	}
	asJSON := f.bools["json"]
	switch {
	case f.bools["daemon"] && f.bools["archived"]:
		return usagef("--daemon and --archived cannot be used together")
	case f.bools["archived"]:
		return a.listArchived(asJSON)
	case f.bools["daemon"]:
		return a.listDaemon(asJSON)
	}
	cat, err := jobs.ReadCatalog(a.home)
	if err != nil {
		return err
	}
	reply, err := a.engine().Resolve(context.Background(), resolveJobs(&cat), false, false)
	if err != nil {
		return err
	}
	views := a.overviews(&cat, reply.Results, "")
	if asJSON {
		return printJSON(a.stdout, views)
	}
	if len(views) == 0 {
		fmt.Fprintln(a.stdout, "No jobs found.")
		return nil
	}
	for _, o := range views {
		fmt.Fprintln(a.stdout, summaryLine(o))
	}
	return nil
}

// summaryLine is `name - schedule - status[, next ...][, last ...]`, or the
// file error.
func summaryLine(o overview) string {
	if o.Schedule == nil {
		e := "unknown error"
		if o.Error != nil {
			e = *o.Error
		}
		return fmt.Sprintf("%s - invalid: %s", o.Name, e)
	}
	line := fmt.Sprintf("%s - %s", o.Name, o.Schedule.Describe())
	if o.Status != jobs.StatusManual {
		line += " - " + string(o.Status)
	}
	if o.NextRunAt != nil {
		line += ", next " + localTime(*o.NextRunAt)
	}
	if o.LastRun != nil {
		line += fmt.Sprintf(", last %s %s", o.LastRun.Outcome, localTime(o.LastRun.FinishedAt))
	}
	return line
}

func localTime(t time.Time) string { return t.Local().Format("2006-01-02 15:04 -07:00") }

func (a *app) listArchived(asJSON bool) error {
	archived, err := a.home.Archived()
	if err != nil {
		return err
	}
	if asJSON {
		return printJSON(a.stdout, archived)
	}
	if len(archived) == 0 {
		fmt.Fprintln(a.stdout, "No archived jobs.")
		return nil
	}
	for _, item := range archived {
		when := "unknown run_at"
		if item.ScheduledFor != nil {
			when = localTime(*item.ScheduledFor)
		}
		if last := item.LastRun; last != nil {
			fmt.Fprintf(a.stdout, "%s - once %s - %s (%s) - %s\n", item.Name, when, last.Outcome, last.Trigger, item.Path)
			if last.Error != nil {
				fmt.Fprintf(a.stdout, "  error: %s\n", *last.Error)
			}
			continue
		}
		fmt.Fprintf(a.stdout, "%s - once %s - outcome unknown - %s\n", item.Name, when, item.Path)
	}
	return nil
}

// daemonJobs is `zdx jobs list --daemon --json`.
type daemonJobs struct {
	Daemon json.RawMessage `json:"daemon"`
	Jobs   []overview      `json:"jobs"`
	Error  *string         `json:"error"`
}

func (a *app) listDaemon(asJSON bool) error {
	view := daemonJobs{Jobs: []overview{}}
	cat, catErr := jobs.ReadCatalog(a.home)
	var resolve []jobs.ResolveJob
	if catErr == nil {
		resolve = resolveJobs(&cat)
	}
	reply, err := a.engine().Resolve(context.Background(), resolve, true, false)
	switch {
	case err != nil:
		reason := "cannot resolve daemon settings: " + err.Error()
		view.Daemon, _ = json.Marshal(map[string]string{"status": "unavailable", "reason": reason})
		if catErr == nil {
			view.Jobs = a.overviews(&cat, nil, reason)
		}
	default:
		view.Daemon = reply.Daemon
		if catErr == nil {
			view.Jobs = a.overviews(&cat, reply.Results, "")
		}
	}
	if catErr != nil {
		text := "discover jobs: " + catErr.Error()
		view.Error = &text
	}
	if asJSON {
		return printJSON(a.stdout, view)
	}

	var state struct {
		Status  string `json:"status"`
		Reason  string `json:"reason"`
		Context struct {
			Source string  `json:"source"`
			Root   string  `json:"root"`
			Note   *string `json:"note"`
		} `json:"context"`
	}
	_ = json.Unmarshal(view.Daemon, &state)
	if state.Status == "available" {
		source := "running daemon"
		if state.Context.Source == "installed" {
			source = "installed daemon service"
		}
		fmt.Fprintf(a.stdout, "Daemon: %s, root %s\n", source, state.Context.Root)
		if state.Context.Note != nil {
			fmt.Fprintf(a.stdout, "Note: %s\n", *state.Context.Note)
		}
	} else {
		fmt.Fprintf(a.stdout, "Daemon: unavailable (%s)\n", state.Reason)
	}
	if view.Error != nil {
		return errors.New(*view.Error)
	}
	if len(view.Jobs) == 0 {
		fmt.Fprintln(a.stdout, "No jobs found.")
	}
	for _, o := range view.Jobs {
		s, ok := o.settings()
		switch {
		case ok:
			fmt.Fprintf(a.stdout, "%s - model=%s [%s] root=%s [%s]\n", summaryLine(o), s.Model.Spec, s.Model.Origin, s.Root.Path, s.Root.Origin)
		case o.Schedule != nil:
			e := "unresolved"
			if o.Error != nil {
				e = *o.Error
			}
			fmt.Fprintf(a.stdout, "%s - error: %s\n", summaryLine(o), e)
		default:
			fmt.Fprintln(a.stdout, summaryLine(o))
		}
	}
	return nil
}

// ---- validate ----

func (a *app) validate(args []string) error {
	if _, err := parseFlags(args, nil, nil, 0); err != nil {
		return err
	}
	cat, err := jobs.ReadCatalog(a.home)
	if err != nil {
		return err
	}
	var problems, lines []string
	for _, inv := range cat.Invalid {
		problems = append(problems, fmt.Sprintf("%s (%s): %s", inv.Name, inv.Path, inv.Error))
	}
	reply, err := a.engine().Resolve(context.Background(), resolveJobs(&cat), false, true)
	if err != nil {
		return err
	}
	views := a.overviews(&jobs.Catalog{Jobs: cat.Jobs}, reply.Results, "")
	byName := map[string]overview{}
	for _, o := range views {
		byName[o.Name] = o
	}
	for i := range cat.Jobs {
		j := &cat.Jobs[i]
		o := byName[j.Name]
		s, ok := o.settings()
		if !ok {
			e := "unresolved"
			if o.Error != nil {
				e = *o.Error
			}
			problems = append(problems, fmt.Sprintf("%s: %s", j.Name, e))
			continue
		}
		lines = append(lines, validationLine(j, s, o.Status))
	}
	if len(problems) > 0 {
		return fmt.Errorf("%d of %d job(s) failed validation:\n- %s", len(problems), len(cat.Jobs)+len(cat.Invalid), strings.Join(problems, "\n- "))
	}
	fmt.Fprintf(a.stdout, "Validated %d job(s).\n", len(cat.Jobs))
	for _, line := range lines {
		fmt.Fprintln(a.stdout, line)
	}
	return nil
}

func validationLine(j *jobs.Definition, s settingsView, status jobs.Status) string {
	grace := "-"
	if g, ok := j.EffectiveGraceSecs(); ok {
		grace = strconv.FormatUint(uint64(g), 10)
	}
	subagent := "<default>"
	if j.Subagent != nil {
		subagent = *j.Subagent
	}
	timeout := "<default>"
	if j.TimeoutSecs != nil {
		timeout = strconv.FormatUint(uint64(*j.TimeoutSecs), 10)
	}
	return fmt.Sprintf("- %s (user): schedule=%s, status=%s, grace_secs=%s, model=%s [%s], root=%s [%s], subagent=%s, timeout_secs=%s, max_retries=%d",
		j.Name, j.Schedule.Describe(), status, grace, s.Model.Spec, s.Model.Origin, s.Root.Path, s.Root.Origin, subagent, timeout, j.MaxRetries)
}

// ---- runs ----

func (a *app) runs(args []string) error {
	f, err := parseFlags(args, []string{"json"}, []string{"date", "date-start", "date-end"}, 1)
	if err != nil {
		return err
	}
	var dates [3]*time.Time
	for i, name := range []string{"date", "date-start", "date-end"} {
		if dates[i], err = parseDateFilter(f.values[name], name); err != nil {
			return err
		}
	}
	exact, start, end := dates[0], dates[1], dates[2]
	if start != nil && end != nil && start.After(*end) {
		return errors.New("--date-start must be on or before --date-end")
	}
	records, err := a.home.ReadRunRecords(a.log)
	if err != nil {
		return err
	}
	var name *string
	if len(f.pos) == 1 {
		n := strings.TrimSpace(f.pos[0])
		name = &n
	}
	filtered := []jobs.RunRecord{}
	for _, r := range records {
		if (name == nil || r.Job == *name) && matchesDates(r, exact, start, end) {
			filtered = append(filtered, r)
		}
	}
	if f.bools["json"] {
		return printJSON(a.stdout, filtered)
	}
	if len(filtered) == 0 {
		if name != nil {
			fmt.Fprintf(a.stdout, "No runs found for job '%s'.\n", *name)
		} else {
			fmt.Fprintln(a.stdout, "No job runs found.")
		}
		return nil
	}
	for _, r := range filtered {
		outcome := "failed"
		switch {
		case r.Outcome != nil:
			outcome = string(*r.Outcome)
		case r.OK:
			outcome = "ok"
		}
		thread := "-"
		if r.ThreadID != nil {
			thread = *r.ThreadID
		}
		fmt.Fprintf(a.stdout, "%s | %s | %s | %s | %dms | attempt %d/%d | thread %s\n",
			r.FinishedAt, r.Job, r.Trigger, outcome, r.DurationMS, r.Attempt, r.MaxAttempts, thread)
		if r.ScheduledFor != nil {
			fmt.Fprintf(a.stdout, "  scheduled for: %s\n", *r.ScheduledFor)
		}
		if r.Error != nil {
			fmt.Fprintf(a.stdout, "  error: %s\n", *r.Error)
		}
	}
	return nil
}

func parseDateFilter(raw *string, flag string) (*time.Time, error) {
	if raw == nil {
		return nil, nil
	}
	t := strings.TrimSpace(*raw)
	if t == "" {
		return nil, fmt.Errorf("--%s cannot be empty", flag)
	}
	d, err := time.Parse("2006-01-02", t)
	if err != nil {
		return nil, fmt.Errorf("invalid --%s value '%s' (expected YYYY-MM-DD)", flag, t)
	}
	return &d, nil
}

func matchesDates(r jobs.RunRecord, exact, start, end *time.Time) bool {
	if exact == nil && start == nil && end == nil {
		return true
	}
	finished, err := time.Parse(time.RFC3339Nano, r.FinishedAt)
	if err != nil {
		return false
	}
	u := finished.UTC()
	day := time.Date(u.Year(), u.Month(), u.Day(), 0, 0, 0, 0, time.UTC)
	return (exact == nil || day.Equal(*exact)) && (start == nil || !day.Before(*start)) && (end == nil || !day.After(*end))
}

// ---- set-model ----

func (a *app) setModel(args []string) error {
	f, err := parseFlags(args, []string{"clear"}, nil, 2)
	if err != nil {
		return err
	}
	if len(f.pos) == 0 {
		return usagef("set-model needs a job name")
	}
	name := f.pos[0]
	var spec *string
	switch {
	case len(f.pos) == 2 && !f.bools["clear"]:
		spec = &f.pos[1]
	case len(f.pos) == 1 && f.bools["clear"]:
	default:
		return errors.New("pass either a model spec or --clear")
	}
	job, err := jobs.LoadByName(a.home, name)
	if err != nil {
		return fmt.Errorf("load job '%s': %w", name, err)
	}
	updated, err := jobs.SetModel(job.Path, spec)
	if err != nil {
		return err
	}
	if updated.Model != nil {
		fmt.Fprintf(a.stdout, "%s: model = %s\n", name, *updated.Model)
	} else {
		fmt.Fprintf(a.stdout, "%s: model cleared (inherits)\n", name)
	}
	return nil
}

// ---- run ----

func (a *app) runJob(args []string) error {
	f, err := parseFlags(args, []string{"keep"}, []string{"occurrence", "scheduled-for"}, 1)
	if err != nil {
		return err
	}
	if len(f.pos) != 1 {
		return usagef("run needs a job name")
	}
	req := jobs.RunRequest{Name: f.pos[0], Keep: f.bools["keep"], ThreadID: a.g.thread, NoThread: a.g.noThread}
	key, at := f.values["occurrence"], f.values["scheduled-for"]
	if (key == nil) != (at == nil) {
		return usagef("--occurrence and --scheduled-for go together")
	}
	if key != nil {
		t, err := time.Parse(time.RFC3339Nano, *at)
		if err != nil {
			return fmt.Errorf("invalid --scheduled-for '%s'", *at)
		}
		req.Scheduled = &jobs.Occurrence{At: t.UTC(), Key: *key}
	}

	interrupted := make(chan struct{})
	forward := make(chan os.Signal, 4)
	signals := make(chan os.Signal, 4)
	signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(signals)
	go func() {
		var once sync.Once
		for sig := range signals {
			once.Do(func() { close(interrupted) })
			// A terminal's Ctrl-C reaches the attempt too (same process
			// group): passing it on would count as a second interrupt.
			if sig == os.Interrupt && terminalForeground() {
				continue
			}
			select {
			case forward <- sig:
			default:
			}
		}
	}()

	runner := jobs.Runner{Home: a.home, Engine: a.engine(), Out: a.stdout, Log: a.log,
		Interrupted: interrupted, Signals: forward}
	return runner.Run(req)
}

// ---- daemon ----

func (a *app) daemon(args []string) error {
	f, err := parseFlags(args, nil, []string{"poll-interval-secs"}, 0)
	if err != nil {
		return err
	}
	poll := 30 * time.Second
	if v := f.values["poll-interval-secs"]; v != nil {
		n, err := strconv.ParseUint(*v, 10, 64)
		if err != nil {
			return usagef("invalid --poll-interval-secs %q", *v)
		}
		poll = time.Duration(max(n, 1)) * time.Second
	}
	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("locate the zdx-jobs binary: %w", err)
	}
	configDir, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("read daemon working directory: %w", err)
	}
	files, err := jobs.AcquireDaemon(a.home, a.g.root, configDir)
	if err != nil {
		return err
	}
	defer files.Release()

	child := a.g.childArgs()
	if a.g.zdx != "" {
		child = append(child, "--zdx", a.g.zdx)
	}
	s := &jobs.Scheduler{Home: a.home, Location: time.Local, Log: a.log, Spawn: jobs.ProcessSpawner(exe, child)}
	if err := s.Start(time.Now()); err != nil {
		return err
	}
	a.log.Info("Jobs daemon started", "root", a.g.root, "poll_secs", int(poll.Seconds()), "since", s.State().Since)
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	s.Loop(ctx, poll)
	a.log.Info("Jobs daemon stopped")
	return nil
}
