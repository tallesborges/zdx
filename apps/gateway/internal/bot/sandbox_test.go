package bot

import (
	"os"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Every test here runs in a sandbox (HOME, ZDX_HOME and TMPDIR are fresh
// directories). The real-service tests keep PATH for the service's tools
// and pass the zdx binary by absolute path.
func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run, false)) }
