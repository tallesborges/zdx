package miniapp

import (
	"os"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Every test here runs in a sandbox (HOME, ZDX_HOME and TMPDIR are fresh
// directories). The opt-in real-service tests keep PATH for git and the
// service's tools, and take the zdx binary by absolute path.
func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run, false)) }
