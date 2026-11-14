//go:build !webdist

package webdist

import "io/fs"

// FS is nil: this build embeds no Mini App (built without -tags webdist).
func FS() fs.FS { return nil }
