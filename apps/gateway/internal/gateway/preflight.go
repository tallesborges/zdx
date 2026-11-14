package gateway

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// PreflightResult is what `zdx-gateway preflight --json` prints for `zdx
// service install bot --program`: whether this build can serve as the bot
// now. Problems make it not ready; warnings do not.
type PreflightResult struct {
	Service string `json:"service"`
	Ready   bool   `json:"ready"`
	// Zdx is the engine binary the gateway runs for settings and service
	// commands, as resolved here.
	Zdx      string   `json:"zdx"`
	Problems []string `json:"problems"`
	Warnings []string `json:"warnings"`
}

// PreflightOptions are the inputs of Preflight.
type PreflightOptions struct {
	Home, Root, Zdx string
	// ZdxFrom is where Zdx came from: --zdx, ZDX_BIN or PATH.
	ZdxFrom string
	Env     []string
	WebDist string
	// Socket overrides engine.SocketPath(Home).
	Socket string
}

// Preflight checks, without changing anything, what serve needs: no
// missing parts, the Rust-resolved settings, the Mini App build when the
// server is enabled, and the orchestrator service capabilities. It takes no
// pidfile, does not contact Telegram, and runs no service command; it reads
// settings through `zdx telegram settings` and asks the service its status.
func Preflight(ctx context.Context, o PreflightOptions) PreflightResult {
	r := PreflightResult{Service: ServiceName, Zdx: o.Zdx, Problems: []string{}, Warnings: []string{}}
	if fi, err := os.Stat(o.Zdx); err != nil || fi.IsDir() || fi.Mode()&0o111 == 0 {
		r.Problems = append(r.Problems, "engine binary "+o.Zdx+" is not an executable file")
	}
	if o.ZdxFrom == "PATH" {
		r.Warnings = append(r.Warnings, "engine binary "+o.Zdx+" was found on this PATH; the service may resolve another one (pass --zdx or set ZDX_BIN)")
	}
	for _, m := range Missing {
		r.Problems = append(r.Problems, "missing: "+m)
	}
	sctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	settings, err := LoadSettings(sctx, o.Zdx, o.Root, o.Env)
	cancel()
	if err != nil {
		r.Problems = append(r.Problems, "settings: "+err.Error())
	} else if settings.Server.Enabled {
		if _, err := MiniAppAssets(o.WebDist); err != nil {
			r.Problems = append(r.Problems, "mini app: "+err.Error())
		}
	}
	socket := o.Socket
	if socket == "" {
		socket = engine.SocketPath(o.Home)
	}
	sctx, cancel = context.WithTimeout(ctx, 10*time.Second)
	report, err := engine.NewClient(socket).StatusReport(sctx, nil)
	cancel()
	switch {
	case err != nil:
		// The orchestrator service may be installed after the bot; the
		// gateway waits for it and answers nothing until it runs.
		r.Warnings = append(r.Warnings, "orchestrator service not reachable: "+err.Error())
	default:
		for _, c := range ServiceCapabilities {
			if !report.Has(c) {
				r.Problems = append(r.Problems, "orchestrator service lacks capability "+c+"; update and restart it")
			}
		}
		// Mini App views (monitor, usage, Git) read the service's root; a
		// gateway on another root would show another project's answers.
		switch {
		case report.ViewsRoot == nil:
			r.Problems = append(r.Problems, "orchestrator service reports no views_root; update and restart it")
		case !sameDir(*report.ViewsRoot, o.Root):
			r.Problems = append(r.Problems, fmt.Sprintf("orchestrator service runs with root %s, not %s; install both services from the same root", *report.ViewsRoot, o.Root))
		}
	}
	r.Ready = len(r.Problems) == 0
	return r
}

// sameDir: a and b name the same directory (symlinks resolved, so /var and
// /private/var match on macOS).
func sameDir(a, b string) bool {
	canon := func(p string) string {
		if abs, err := filepath.Abs(p); err == nil {
			p = abs
		}
		if real, err := filepath.EvalSymlinks(p); err == nil {
			return real
		}
		return filepath.Clean(p)
	}
	return canon(a) == canon(b)
}
