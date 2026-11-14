package protocol

import (
	"encoding/json"
	"fmt"
	"time"
)

// View ops, as crates/zdx-engine/src/orchestration/views.rs names them.
const (
	ViewThreadsList       = "threads_list"
	ViewThreadView        = "thread_view"
	ViewThreadTrajectory  = "thread_trajectory"
	ViewThreadWorkers     = "thread_workers"
	ViewArtifactsList     = "artifacts_list"
	ViewArtifactResolve   = "artifact_resolve"
	ViewArtifactVersion   = "artifact_version"
	ViewArtifactCanonical = "artifact_canonical"
	ViewMonitorSnapshot   = "monitor_snapshot"
	ViewActiveAgents      = "active_agents"
	ViewWorkspaces        = "workspaces"
	ViewUsageReport       = "usage_report"
	ViewGitStatus         = "git_status"
	ViewGitScope          = "git_scope"
	ViewGitDiff           = "git_diff"
	ViewThreadRoot        = "thread_root"
)

// viewCapability is views.rs capability(): the capability each view needs.
var viewCapability = map[string]string{
	ViewThreadsList: CapThreadViews, ViewThreadView: CapThreadViews,
	ViewThreadTrajectory: CapThreadViews, ViewThreadWorkers: CapThreadViews,
	ViewArtifactsList: CapArtifactViews, ViewArtifactResolve: CapArtifactViews,
	ViewArtifactVersion: CapArtifactViews, ViewArtifactCanonical: CapArtifactViews,
	ViewMonitorSnapshot: CapMonitorViews, ViewActiveAgents: CapMonitorViews,
	ViewWorkspaces:  CapMonitorViews,
	ViewUsageReport: CapMonitorViews,
	ViewGitStatus:   CapGitViews, ViewGitScope: CapGitViews, ViewGitDiff: CapGitViews,
	ViewThreadRoot: CapThreadRoot,
}

// ViewCapability is the capability a view op needs, or "" for an unknown op.
func ViewCapability(op string) string { return viewCapability[op] }

// CanonicalTimeout is artifact_canonical's reply budget: Rust's image
// normalization waits up to 60 seconds for a slot
// (core/artifact_feedback/canonical.rs SLOT_WAIT) before it decodes and
// encodes.
const CanonicalTimeout = 2 * time.Minute

// ViewBudget is the reply budget of op: its own budget, else fallback.
func ViewBudget(op string, fallback time.Duration) time.Duration {
	switch op {
	case ViewArtifactCanonical:
		return CanonicalTimeout
	}
	return fallback
}

// View is one read-only view request (views.rs ViewRequest). Op names the
// view; set only the fields that view takes. Kind is threads_list's kind or
// git_diff's kind.
type View struct {
	Op       string  `json:"-"`
	ThreadID *string `json:"thread_id,omitempty"`
	Kind     *string `json:"kind,omitempty"`
	After    *uint64 `json:"after,omitempty"`
	Path     *string `json:"path,omitempty"`
	Scope    *string `json:"scope,omitempty"`
	Commit   *string `json:"commit,omitempty"`
	Days     *uint32 `json:"days,omitempty"`
}

// Encode is the request line: the fields tagged with `op`.
func (v View) Encode() ([]byte, error) { return EncodeLine(v.Op, v) }

// Output is a view's result: the exact JSON body of the Mini App route, and
// for thread_view the thread the events came from.
type Output struct {
	Body     json.RawMessage
	SourceID *string
}

// DecodeViewReply reads a `{"reply":"view",...}` line: the socket reply to
// a view, and the stdout of `zdx views`. An error reply is a *ServiceError.
func DecodeViewReply(line []byte, op string) (Output, error) {
	var r Reply
	if err := json.Unmarshal(line, &r); err != nil {
		return Output{}, fmt.Errorf("decode reply to %s: %w", op, err)
	}
	if err := r.Err(); err != nil {
		return Output{}, err
	}
	if r.Kind != ReplyView || len(r.Body) == 0 {
		return Output{}, fmt.Errorf("unexpected reply %q to %s", r.Kind, op)
	}
	return Output{Body: r.Body, SourceID: r.SourceID}, nil
}
