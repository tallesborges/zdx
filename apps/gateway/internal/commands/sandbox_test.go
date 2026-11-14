package commands

import (
	"os"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Every test here runs in a sandbox: HOME, ZDX_HOME and TMPDIR are fresh
// directories, and PATH holds a zdx that fails. The tests use fakes only
// and run no tools, so a future test that reaches the real home or the real
// zdx fails instead of touching them.
func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run, true)) }
