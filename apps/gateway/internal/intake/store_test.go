package intake

import (
	"os"
	"path/filepath"
	"testing"
)

// Matches Rust's Path::new(p).file_name().and_then(to_str).
func TestFileNameFromPathMatchesRust(t *testing.T) {
	cases := map[string]string{
		"photos/file_1.jpg": "file_1.jpg",
		"report.pdf":        "report.pdf",
		"../../etc/passwd":  "passwd",
		"dir/":              "dir",
		"foo/.":             "foo",
		"foo/./":            "foo",
		"a//b":              "b",
		"":                  "",
		".":                 "",
		"..":                "",
		"foo/..":            "",
		"/":                 "",
		"/.":                "",
		"bad\xffname":       "",
	}
	for in, want := range cases {
		got, ok := fileNameFromPath(in)
		if got != want || ok != (want != "") {
			t.Errorf("fileNameFromPath(%q) = %q, %v; want %q", in, got, ok, want)
		}
	}
}

func TestDirStoreLayout(t *testing.T) {
	root := filepath.Join(t.TempDir(), "telegram")
	s := DirStore{Root: root}
	path, err := s.Save(-1001234, 7, "sub/clip.ogg", []byte("one"))
	if err != nil {
		t.Fatal(err)
	}
	if want := filepath.Join(root, "-1001234", "7_clip.ogg"); path != want {
		t.Fatalf("path %q, want %q", path, want)
	}
	if _, err := s.Save(-1001234, 7, "clip.ogg", []byte("two")); err != nil {
		t.Fatal(err)
	}
	if data, _ := os.ReadFile(path); string(data) != "two" {
		t.Fatalf("not replaced: %q", data)
	}
	if path, err := s.Save(1, 2, "..", []byte("x")); err != nil || path != filepath.Join(root, "1", "2_..") {
		t.Fatalf("Rust keeps the raw name when it has no file name: %q, %v", path, err)
	}
	if _, err := (DirStore{Root: "telegram"}).Save(1, 2, "a", nil); err == nil {
		t.Fatal("relative root accepted")
	}
}
