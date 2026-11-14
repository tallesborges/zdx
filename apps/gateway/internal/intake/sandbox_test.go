package intake

import (
	"os"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Every test here runs in a sandbox (HOME, ZDX_HOME and TMPDIR are fresh
// directories). Intake runs no binary, so PATH gets the failing fake zdx.
func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run, true)) }
