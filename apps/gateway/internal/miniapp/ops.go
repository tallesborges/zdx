package miniapp

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"
)

const (
	// monitorCacheTTL is how long the local monitor snapshot and each usage
	// window are reused (server.rs MONITOR_CACHE_TTL).
	monitorCacheTTL = 30 * time.Second
	// subscriptionCacheTTL is how old quotas may get before a background
	// refresh starts (server.rs SUBSCRIPTION_CACHE_TTL).
	subscriptionCacheTTL = 5 * time.Minute
	defaultUsageDays     = 7
	maxUsageDays         = 365
)

type monitorCache struct {
	// build is held across a snapshot build, so concurrent requests on an
	// expired cache share one build.
	build    sync.Mutex
	mu       sync.Mutex
	snapshot json.RawMessage
	builtAt  time.Time

	quotaMu     sync.Mutex
	quotas      json.RawMessage
	quotasAt    time.Time
	haveQuotas  bool
	quotaFlight sync.Mutex
}

func (c *monitorCache) fresh() (json.RawMessage, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.snapshot != nil && time.Since(c.builtAt) < monitorCacheTTL {
		return c.snapshot, true
	}
	return nil, false
}

// getMonitor serves the dashboard: the cached local snapshot, active agents
// read fresh, and the last known quotas. Quotas are never awaited here: a
// fetch calls every stored account, so it runs in the background and
// `subscriptions_status` tells an empty `pending` list from "none".
func (s *server) getMonitor(w http.ResponseWriter, r *http.Request, _ call) {
	snapshot, err := s.monitorSnapshot(r.Context())
	if err != nil {
		s.viewFailure(w, err, "Monitor", "Monitor snapshot unavailable")
		return
	}
	agents, err := s.engine.ActiveAgents(r.Context())
	if err != nil {
		s.viewFailure(w, err, "Monitor", "Active agents unavailable")
		return
	}

	quotas, age, known := s.cachedQuotas()
	status, refresh := classifySubscriptions(age, known)
	subscriptions, subscriptionsAge := any(json.RawMessage("[]")), any(nil)
	if known {
		subscriptions, subscriptionsAge = quotas, formatUptime(age)
	}
	if refresh {
		s.refreshQuotas()
	}
	body, err := setFields(snapshot,
		field{"active_agents", agents},
		field{"subscriptions", subscriptions},
		field{"subscriptions_status", status},
		field{"subscriptions_age", subscriptionsAge},
	)
	if err != nil {
		s.log.Warn("Monitor snapshot is not a JSON object", "error", err)
		writeText(w, http.StatusInternalServerError, "Monitor snapshot unavailable")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

func (s *server) monitorSnapshot(ctx context.Context) (json.RawMessage, error) {
	if snapshot, ok := s.monitor.fresh(); ok {
		return snapshot, nil
	}
	s.monitor.build.Lock()
	defer s.monitor.build.Unlock()
	if snapshot, ok := s.monitor.fresh(); ok {
		return snapshot, nil
	}
	snapshot, err := s.engine.MonitorSnapshot(ctx)
	if err != nil {
		return nil, err
	}
	jobs, jobsError := s.engine.JobSummaries(ctx)
	if snapshot, err = setFields(snapshot, field{"jobs", jobs}, field{"jobs_error", jobsError}); err != nil {
		return nil, fmt.Errorf("monitor snapshot: %w", err)
	}
	s.monitor.mu.Lock()
	s.monitor.snapshot, s.monitor.builtAt = snapshot, time.Now()
	s.monitor.mu.Unlock()
	return snapshot, nil
}

func (s *server) cachedQuotas() (json.RawMessage, time.Duration, bool) {
	s.monitor.quotaMu.Lock()
	defer s.monitor.quotaMu.Unlock()
	if !s.monitor.haveQuotas {
		return nil, 0, false
	}
	return s.monitor.quotas, time.Since(s.monitor.quotasAt), true
}

// classifySubscriptions maps the quota snapshot age to the reported status
// and whether a refresh is due. Never fetched is `pending`, which must stay
// distinguishable from a fetched empty list.
func classifySubscriptions(age time.Duration, known bool) (string, bool) {
	switch {
	case !known:
		return "pending", true
	case age < subscriptionCacheTTL:
		return "ready", false
	default:
		return "stale", true
	}
}

// refreshQuotas starts one background quota fetch unless one runs. A failed
// fetch keeps the previous state (`pending` or `stale`) instead of caching an
// empty list that would read as "no subscriptions".
func (s *server) refreshQuotas() {
	if !s.monitor.quotaFlight.TryLock() {
		return
	}
	go func() {
		defer s.monitor.quotaFlight.Unlock()
		started := time.Now()
		quotas, err := s.engine.SubscriptionQuotas(context.Background())
		if err != nil {
			s.log.Warn("Failed to refresh Mini App subscription quotas", "error", err)
			return
		}
		if len(quotas) == 0 {
			quotas = json.RawMessage("[]")
		}
		s.log.Debug("Refreshed Mini App subscription quotas", "elapsed_ms", time.Since(started).Milliseconds())
		s.monitor.quotaMu.Lock()
		s.monitor.quotas, s.monitor.quotasAt, s.monitor.haveQuotas = quotas, time.Now(), true
		s.monitor.quotaMu.Unlock()
	}()
}

type cachedUsage struct {
	at   time.Time
	body json.RawMessage
}

// usageCache holds usage reports per window. The lock is held across a
// build, so concurrent requests share one usage-cache sync in the engine.
type usageCache struct {
	mu      sync.Mutex
	entries map[int]cachedUsage
}

// getUsage serves recorded usage for the last `?days=` days (default 7,
// 1-365), cached per window.
func (s *server) getUsage(w http.ResponseWriter, r *http.Request, _ call) {
	query, ok := parseQuery(w, r, queryField{name: "days", kind: queryU32})
	if !ok {
		return
	}
	days := defaultUsageDays
	if n, ok := query.number("days"); ok {
		days = int(n)
	}
	if days < 1 || days > maxUsageDays {
		writeText(w, http.StatusBadRequest, "days must be between 1 and 365")
		return
	}
	s.usage.mu.Lock()
	defer s.usage.mu.Unlock()
	if cached, ok := s.usage.entries[days]; ok && time.Since(cached.at) < monitorCacheTTL {
		writeRawJSON(w, http.StatusOK, cached.body)
		return
	}
	body, err := s.engine.UsageReport(r.Context(), days)
	if err != nil {
		s.viewFailure(w, err, "Usage", "Usage unavailable")
		return
	}
	s.usage.entries[days] = cachedUsage{time.Now(), body}
	writeRawJSON(w, http.StatusOK, body)
}

// getWorkspaces serves read-only model settings of every workspace. Not
// cached: it reads a few small TOML files.
func (s *server) getWorkspaces(w http.ResponseWriter, r *http.Request, _ call) {
	body, err := s.engine.Workspaces(r.Context())
	if err != nil {
		s.viewFailure(w, err, "Workspaces", "Workspaces unavailable")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

// gitThreadID checks the optional `thread_id` of a Git route: blank means
// none (the engine then uses `active`), anything else must be a valid id.
func gitThreadID(w http.ResponseWriter, query queryValues) (string, bool) {
	id, _ := query.text("thread_id")
	if blank(id) {
		return "", true
	}
	if id != "active" && !validThreadID(id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return "", false
	}
	return id, true
}

// gitFailure answers a failed Git view. The engine owns Git inspection,
// its input checks and its texts (the gateway's engine runs the shared
// gitview package on Rust's thread_root view).
func (s *server) gitFailure(w http.ResponseWriter, err error) {
	s.viewFailure(w, err, "Git views", "Git state unavailable")
}

func (s *server) getGit(w http.ResponseWriter, r *http.Request, _ call) {
	query, ok := parseQuery(w, r, queryField{name: "thread_id"})
	if !ok {
		return
	}
	id, ok := gitThreadID(w, query)
	if !ok {
		return
	}
	body, err := s.engine.GitStatus(r.Context(), id)
	if err != nil {
		s.gitFailure(w, err)
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

func (s *server) getGitScope(w http.ResponseWriter, r *http.Request, _ call) {
	query, ok := parseQuery(w, r, queryField{name: "thread_id"}, queryField{name: "scope", required: true})
	if !ok {
		return
	}
	id, ok := gitThreadID(w, query)
	if !ok {
		return
	}
	scope, _ := query.text("scope")
	body, err := s.engine.GitScope(r.Context(), id, scope)
	if err != nil {
		s.gitFailure(w, err)
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

func (s *server) getGitDiff(w http.ResponseWriter, r *http.Request, _ call) {
	query, ok := parseQuery(w, r,
		queryField{name: "thread_id"},
		queryField{name: "path", required: true},
		queryField{name: "kind", required: true},
		queryField{name: "commit"},
	)
	if !ok {
		return
	}
	// server.rs checks the file path before it resolves the repository; the
	// engine owns that check, so an invalid path reaches it with the id.
	id, ok := gitThreadID(w, query)
	if !ok {
		return
	}
	path, _ := query.text("path")
	kind, _ := query.text("kind")
	commit, _ := query.text("commit")
	body, err := s.engine.GitDiff(r.Context(), GitDiff{ThreadID: id, Path: path, Kind: kind, Commit: commit})
	if err != nil {
		s.gitFailure(w, err)
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}
