// Package jobs owns ZDX jobs: saved prompts that run unattended, manually,
// on a recurring cron schedule, or once at a set time (docs/SPEC.md §8
// Jobs). It parses job files, computes schedules and status, and reads and
// writes the durable state under $ZDX_HOME (claims, locks, last outcomes,
// the run log, the archive). The scheduler (Scheduler) and the run
// bookkeeping (Runner) live here too; the agent of each attempt and the
// settings resolution are Rust (`zdx jobs attempt`, `zdx jobs resolve`),
// reached through Engine.
package jobs

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

// Home is one ZDX home, the root of every job file.
type Home struct{ Dir string }

// JobsDir is `$ZDX_HOME/jobs`: one `<name>.md` per job.
func (h Home) JobsDir() string { return filepath.Join(h.Dir, "jobs") }

// ArchiveDir is `$ZDX_HOME/jobs/archive`: once jobs that reached a final
// outcome.
func (h Home) ArchiveDir() string { return filepath.Join(h.JobsDir(), "archive") }

// RunsLog is `$ZDX_HOME/jobs_runs.jsonl`.
func (h Home) RunsLog() string { return filepath.Join(h.Dir, "jobs_runs.jsonl") }

func (h Home) stateDir() string              { return filepath.Join(h.Dir, "state", "jobs") }
func (h Home) jobStateDir(job string) string { return filepath.Join(h.stateDir(), job) }
func (h Home) claimPath(job, key string) string {
	return filepath.Join(h.jobStateDir(job), "claims", key+".json")
}

// Definition is one parsed job file.
type Definition struct {
	// Name is the file stem.
	Name     string
	Path     string
	Schedule Schedule
	// GraceSecs is the declared value; EffectiveGraceSecs applies the
	// default.
	GraceSecs *uint32
	Model     *string
	Subagent  *string
	// Root is as written (`~/...` or absolute); Rust expands and checks it
	// at resolution, so a bad root fails only this job.
	Root        *string
	TimeoutSecs *uint32
	MaxRetries  uint32
	Prompt      string
}

// EffectiveGraceSecs is the grace in seconds; false for manual jobs.
func (d *Definition) EffectiveGraceSecs() (uint32, bool) {
	if d.Schedule.Kind == Manual {
		return 0, false
	}
	if d.GraceSecs != nil {
		return *d.GraceSecs, true
	}
	return d.Schedule.DefaultGraceSecs()
}

// Grace is EffectiveGraceSecs as a duration.
func (d *Definition) Grace() (time.Duration, bool) {
	secs, ok := d.EffectiveGraceSecs()
	return time.Duration(secs) * time.Second, ok
}

// Invalid is a job file that does not parse. It fails alone: the other jobs
// still run.
type Invalid struct {
	Name  string `json:"name"`
	Path  string `json:"path"`
	Error string `json:"error"`
}

// Catalog is every job file in `$ZDX_HOME/jobs`, split into valid jobs and
// invalid files, each sorted by name.
type Catalog struct {
	Jobs    []Definition
	Invalid []Invalid
}

// Find returns a valid job by name, or why it cannot run.
func (c *Catalog) Find(name string) (*Definition, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return nil, fmt.Errorf("Job name cannot be empty")
	}
	for i := range c.Jobs {
		if c.Jobs[i].Name == name {
			return &c.Jobs[i], nil
		}
	}
	for _, inv := range c.Invalid {
		if inv.Name == name {
			return nil, fmt.Errorf("job '%s' is invalid (%s): %s", name, inv.Path, inv.Error)
		}
	}
	return nil, fmt.Errorf("Job '%s' not found", name)
}

// ReadCatalog reads every job file in home. A file that does not parse is
// reported in Catalog.Invalid and does not affect the others.
func ReadCatalog(home Home) (Catalog, error) {
	files, err := markdownFiles(home.JobsDir())
	if err != nil {
		return Catalog{}, err
	}
	var cat Catalog
	byName := map[string]string{}
	for _, path := range files {
		name, err := fileStem(path)
		if err != nil {
			name = path
		}
		def, err := ParseFile(path)
		if err != nil {
			cat.Invalid = append(cat.Invalid, Invalid{Name: name, Path: path, Error: err.Error()})
			continue
		}
		if existing, ok := byName[def.Name]; ok {
			cat.Invalid = append(cat.Invalid, Invalid{Name: name, Path: path,
				Error: fmt.Sprintf("duplicate job name: '%s' already defines it", existing)})
			continue
		}
		byName[def.Name] = def.Path
		cat.Jobs = append(cat.Jobs, def)
	}
	sort.SliceStable(cat.Jobs, func(i, j int) bool { return cat.Jobs[i].Name < cat.Jobs[j].Name })
	sort.SliceStable(cat.Invalid, func(i, j int) bool { return cat.Invalid[i].Name < cat.Invalid[j].Name })
	return cat, nil
}

// LoadByName loads one valid job.
func LoadByName(home Home, name string) (*Definition, error) {
	cat, err := ReadCatalog(home)
	if err != nil {
		return nil, err
	}
	return cat.Find(name)
}

// markdownFiles lists `*.md` files (any case) directly in dir, sorted. A
// missing dir has none.
func markdownFiles(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read job dir %s: %w", dir, err)
	}
	var out []string
	for _, e := range entries {
		path := filepath.Join(dir, e.Name())
		if !strings.EqualFold(filepath.Ext(e.Name()), ".md") {
			continue
		}
		if info, err := os.Stat(path); err != nil || !info.Mode().IsRegular() {
			continue
		}
		out = append(out, path)
	}
	sort.Strings(out)
	return out, nil
}

func fileStem(path string) (string, error) {
	base := filepath.Base(path)
	stem := strings.TrimSpace(strings.TrimSuffix(base, filepath.Ext(base)))
	if stem == "" {
		return "", fmt.Errorf("Invalid job file name: %s", path)
	}
	return stem, nil
}

// ParseFile reads and parses one job file.
func ParseFile(path string) (Definition, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return Definition{}, fmt.Errorf("read job file %s: %w", path, err)
	}
	return ParseContent(path, string(b))
}

// ParseContent parses a job file's content; path names the job.
func ParseContent(path, content string) (Definition, error) {
	lines, body, err := splitFrontmatter(content)
	if err != nil {
		return Definition{}, err
	}
	fm, err := parseFlatYAML(lines)
	if err != nil {
		return Definition{}, fmt.Errorf("parse YAML frontmatter in %s: %w", path, err)
	}
	var ints [3]*uint32
	for i, key := range []string{"grace_secs", "timeout_secs", "max_retries"} {
		if ints[i], err = uintKey(fm, key); err != nil {
			return Definition{}, fmt.Errorf("parse YAML frontmatter in %s: %w", path, err)
		}
	}
	grace, timeout, retries := ints[0], ints[1], ints[2]
	name, err := fileStem(path)
	if err != nil {
		return Definition{}, err
	}
	cron, err := nonEmpty(fm, "schedule")
	if err != nil {
		return Definition{}, err
	}
	runAt, err := nonEmpty(fm, "run_at")
	if err != nil {
		return Definition{}, err
	}
	schedule, err := ParseSchedule(cron, runAt)
	if err != nil {
		return Definition{}, err
	}
	model, err := nonEmpty(fm, "model")
	if err != nil {
		return Definition{}, err
	}
	subagent, err := nonEmpty(fm, "subagent")
	if err != nil {
		return Definition{}, err
	}
	var root *string
	if v := fm["root"]; v != nil {
		r := strings.TrimSpace(v.text)
		root = &r
	}
	if timeout != nil && *timeout == 0 {
		return Definition{}, fmt.Errorf("timeout_secs must be greater than zero")
	}
	if grace != nil {
		if schedule.Kind == Manual {
			return Definition{}, fmt.Errorf("grace_secs needs `schedule` or `run_at`")
		}
		if *grace < MinGraceSecs || *grace > MaxGraceSecs {
			return Definition{}, fmt.Errorf("grace_secs must be between %d and %d", MinGraceSecs, MaxGraceSecs)
		}
	}
	prompt := strings.TrimSpace(body)
	if prompt == "" {
		return Definition{}, fmt.Errorf("Job prompt body cannot be empty")
	}
	def := Definition{Name: name, Path: path, Schedule: schedule, GraceSecs: grace, Model: model,
		Subagent: subagent, Root: root, TimeoutSecs: timeout, Prompt: prompt}
	if retries != nil {
		def.MaxRetries = *retries
	}
	return def, nil
}

func nonEmpty(fm map[string]*scalar, key string) (*string, error) {
	v := fm[key]
	if v == nil {
		return nil, nil
	}
	t := strings.TrimSpace(v.text)
	if t == "" {
		return nil, fmt.Errorf("%s cannot be empty", key)
	}
	return &t, nil
}

func uintKey(fm map[string]*scalar, key string) (*uint32, error) {
	v := fm[key]
	if v == nil {
		return nil, nil
	}
	n, err := strconv.ParseUint(strings.TrimPrefix(v.text, "+"), 10, 32)
	if v.quoted || err != nil {
		return nil, fmt.Errorf("%s: expected a non-negative integer, got '%s'", key, v.text)
	}
	u := uint32(n)
	return &u, nil
}
