package engine

import (
	"context"

	"github.com/tallesborges/zdx/apps/shared/protocol"
)

// View ops (package protocol, as views.rs names them).
const (
	ViewThreadsList       = protocol.ViewThreadsList
	ViewThreadView        = protocol.ViewThreadView
	ViewThreadTrajectory  = protocol.ViewThreadTrajectory
	ViewThreadWorkers     = protocol.ViewThreadWorkers
	ViewArtifactsList     = protocol.ViewArtifactsList
	ViewArtifactResolve   = protocol.ViewArtifactResolve
	ViewArtifactVersion   = protocol.ViewArtifactVersion
	ViewArtifactCanonical = protocol.ViewArtifactCanonical
	ViewMonitorSnapshot   = protocol.ViewMonitorSnapshot
	ViewActiveAgents      = protocol.ViewActiveAgents
	ViewWorkspaces        = protocol.ViewWorkspaces
	ViewUsageReport       = protocol.ViewUsageReport
	ViewGitStatus         = protocol.ViewGitStatus
	ViewGitScope          = protocol.ViewGitScope
	ViewGitDiff           = protocol.ViewGitDiff
)

// ViewCapability is the capability a view op needs, or "" for an unknown op.
func ViewCapability(op string) string { return protocol.ViewCapability(op) }

// View is one read-only view request (views.rs ViewRequest); see
// protocol.View.
type View = protocol.View

// ViewOutput is a view's result: the exact JSON body of the Mini App route,
// and for thread_view the thread the events came from.
type ViewOutput = protocol.Output

// View runs one view. It checks the view's capability first and sends
// nothing without it (*ErrUnsupported). A refusal is a *ServiceError whose
// Code maps to the route's HTTP status. artifact_canonical gets
// CanonicalTimeout; others the default reply budget.
func (c *Client) View(ctx context.Context, v View) (ViewOutput, error) {
	return c.transport().View(ctx, v)
}
