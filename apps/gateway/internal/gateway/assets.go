package gateway

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/tallesborges/zdx/apps/gateway/internal/gateway/webdist"
)

// MiniAppAssets picks the built Mini App: dir (an apps/web/dist build) when
// set, else the copy embedded at build time. It refuses a source without
// index.html, so the Mini App never starts with a shell that answers 500.
func MiniAppAssets(dir string) (fs.FS, error) {
	var assets fs.FS
	source := "the embedded build"
	switch {
	case dir != "":
		if !filepath.IsAbs(dir) {
			return nil, errors.New("--web-dist must be an absolute path")
		}
		assets, source = os.DirFS(dir), dir
	default:
		assets = webdist.FS()
		if assets == nil {
			return nil, errors.New("this build embeds no Mini App: build it with apps/gateway/build.sh, or pass --web-dist <apps/web/dist>")
		}
	}
	info, err := fs.Stat(assets, "index.html")
	if err != nil || !info.Mode().IsRegular() {
		return nil, fmt.Errorf("Mini App assets in %s have no index.html; run `just web-build`", source)
	}
	return assets, nil
}
