package intake

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"unicode/utf8"
)

// DirStore saves attachments as the Rust bot does: Root/<chat>/<message>_<name>,
// where Root is $ZDX_HOME/telegram. An existing file of the same name is
// replaced, so loading a message again rewrites the same paths.
type DirStore struct {
	Root string
}

// Save implements Store.
func (s DirStore) Save(chat, message int64, fileName string, data []byte) (string, error) {
	if !filepath.IsAbs(s.Root) {
		return "", errors.New("attachment store root must be an absolute path")
	}
	dir := filepath.Join(s.Root, strconv.FormatInt(chat, 10))
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return "", fmt.Errorf("create media directory: %w", err)
	}
	name, ok := fileNameFromPath(fileName)
	if !ok {
		name = fileName
	}
	path := filepath.Join(dir, strconv.FormatInt(message, 10)+"_"+name)
	if filepath.Dir(path) != dir {
		return "", errors.New("attachment file name leaves the media directory")
	}
	if err := os.WriteFile(path, data, 0o666); err != nil {
		return "", fmt.Errorf("write media file: %w", err)
	}
	return path, nil
}

// fileNameFromPath is Rust's Path::file_name on a '/'-separated path: the
// last normal component, ignoring trailing slashes and "." components; none
// for "", ".", "..", a root, or text that is not UTF-8.
func fileNameFromPath(p string) (string, bool) {
	for {
		t := strings.TrimRight(p, "/")
		if t == "." {
			return "", false
		}
		if rest, ok := strings.CutSuffix(t, "/."); ok {
			p = rest
			continue
		}
		p = t
		break
	}
	name := p[strings.LastIndexByte(p, '/')+1:]
	if name == "" || name == "." || name == ".." || !utf8.ValidString(name) {
		return "", false
	}
	return name, true
}
