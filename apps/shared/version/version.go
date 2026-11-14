// Package version is the zdx version a Go program was built for: the
// workspace version in Cargo.toml, the same value as Rust's
// CARGO_PKG_VERSION. Builds set it with the linker flag that
// apps/shared/ldflags.sh prints. A plain `go build`, `go run` or `go test`
// leaves it empty; Resolve then fails with what to do.
package version

import "errors"

// Zdx is set at build time:
// -ldflags "-X github.com/tallesborges/zdx/apps/shared/version.Zdx=<version>".
var Zdx string

// ErrUnknown: neither an explicit version nor a build-time one is set.
var ErrUnknown = errors.New("zdx version unknown: this binary was built without the version linker flag; " +
	"build it with its just recipe, add -ldflags \"$(apps/shared/ldflags.sh)\" to go build or go run " +
	"(from apps/monitor: ../shared/ldflags.sh), or pass --zdx-version")

// Resolve is explicit when set (a --zdx-version flag), else Zdx, else
// ErrUnknown. It never guesses a version.
func Resolve(explicit string) (string, error) {
	switch {
	case explicit != "":
		return explicit, nil
	case Zdx != "":
		return Zdx, nil
	}
	return "", ErrUnknown
}
