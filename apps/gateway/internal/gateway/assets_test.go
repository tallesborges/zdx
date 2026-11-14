package gateway

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/gateway/webdist"
)

func TestMiniAppAssetsRefuseAMissingBuild(t *testing.T) {
	if webdist.FS() == nil {
		if _, err := MiniAppAssets(""); err == nil || !strings.Contains(err.Error(), "embeds no Mini App") {
			t.Fatalf("a build without -tags webdist has no assets: %v", err)
		}
	} else if _, err := MiniAppAssets(""); err != nil {
		t.Fatalf("the embedded build serves index.html: %v", err)
	}
	dir := t.TempDir()
	if _, err := MiniAppAssets(dir); err == nil || !strings.Contains(err.Error(), "index.html") {
		t.Fatalf("a dir without index.html is refused: %v", err)
	}
	if _, err := MiniAppAssets("relative/dist"); err == nil {
		t.Fatal("a relative dir is refused")
	}
	os.WriteFile(filepath.Join(dir, "index.html"), []byte("<html></html>"), 0o600)
	if a, err := MiniAppAssets(dir); err != nil || a == nil {
		t.Fatalf("a built dir is used: %v", err)
	}
}
