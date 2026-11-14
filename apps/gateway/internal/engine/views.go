package engine

import (
	"context"
	"fmt"
	"time"
)

// View ops, as crates/zdx-engine/src/orchestration/views.rs names them.
const (
	ViewThreadsList        = "threads_list"
	ViewThreadView         = "thread_view"
	ViewThreadTrajectory   = "thread_trajectory"
	ViewThreadWorkers      = "thread_workers"
	ViewArtifactsList      = "artifacts_list"
	ViewArtifactResolve    = "artifact_resolve"
	ViewArtifactVersion    = "artifact_version"
	ViewArtifactCanonical  = "artifact_canonical"
	ViewMonitorSnapshot    = "monitor_snapshot"
	ViewActiveAgents       = "active_agents"
	ViewSubscriptionQuotas = "subscription_quotas"
	ViewWorkspaces         = "workspaces"
	ViewUsageReport        = "usage_report"
	ViewGitStatus          = "git_status"
	ViewGitScope           = "git_scope"
	ViewGitDiff            = "git_diff"
)

// viewCapability is views.rs capability(): the capability each view needs.
var viewCapability = map[string]string{
	ViewThreadsList: CapThreadViews, ViewThreadView: CapThreadViews,
	ViewThreadTrajectory: CapThreadViews, ViewThreadWorkers: CapThreadViews,
	ViewArtifactsList: CapArtifactViews, ViewArtifactResolve: CapArtifactViews,
	ViewArtifactVersion: CapArtifactViews, ViewArtifactCanonical: CapArtifactViews,
	ViewMonitorSnapshot: CapMonitorViews, ViewActiveAgents: CapMonitorViews,
	ViewSubscriptionQuotas: CapMonitorViews, ViewWorkspaces: CapMonitorViews,
	ViewUsageReport: CapMonitorViews,
	ViewGitStatus:   CapGitViews, ViewGitScope: CapGitViews, ViewGitDiff: CapGitViews,
}

// ViewCapability is the capability a view op needs, or "" for an unknown op.
func ViewCapability(op string) string { return viewCapability[op] }

// QuotasTimeout is the reply budget of subscription_quotas: Rust asks every
// provider account over the network.
const QuotasTimeout = 2 * time.Minute

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

func (v View) op() string { return v.Op }

// ViewOutput is a view's result: the exact JSON body of the Mini App route,
// and for thread_view the thread the events came from.
type ViewOutput struct {
	Body     []byte
	SourceID *string
}

// View runs one view. It checks the view's capability first and sends
// nothing without it (*ErrUnsupported). A refusal is a *ServiceError whose
// Code maps to the route's HTTP status. artifact_canonical gets
// CanonicalTimeout and subscription_quotas QuotasTimeout; others the
// default reply budget.
func (c *Client) View(ctx context.Context, v View) (ViewOutput, error) {
	capability := viewCapability[v.Op]
	if capability == "" {
		return ViewOutput{}, fmt.Errorf("%w: unknown view %q", ErrRequestNotSent, v.Op)
	}
	if err := c.Require(ctx, capability); err != nil {
		return ViewOutput{}, err
	}
	budget := c.timeout()
	switch v.Op {
	case ViewArtifactCanonical:
		budget = CanonicalTimeout
	case ViewSubscriptionQuotas:
		budget = QuotasTimeout
	}
	reply, err := c.CallWithin(ctx, v, budget)
	if err != nil {
		return ViewOutput{}, err
	}
	if err := reply.Err(); err != nil {
		return ViewOutput{}, err
	}
	if reply.Kind != ReplyView || len(reply.Body) == 0 {
		return ViewOutput{}, fmt.Errorf("unexpected reply %q to %s", reply.Kind, v.Op)
	}
	return ViewOutput{Body: reply.Body, SourceID: reply.SourceID}, nil
}
