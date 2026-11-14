//go:build webdist

package webdist

import (
	"embed"
	"io/fs"
)

//go:embed all:dist
var files embed.FS

// FS is the embedded Mini App build, rooted at dist.
func FS() fs.FS {
	sub, err := fs.Sub(files, "dist")
	if err != nil {
		panic(err)
	}
	return sub
}
