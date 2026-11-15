package transcript

import (
	"os"
	"testing"

	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func TestCleanTextRemovesTerminalControl(t *testing.T) {
	cases := map[string]string{
		"plain\ttext\nnext":                                   "plain\ttext\nnext",
		"ünïcode 漢字":                                          "ünïcode 漢字",
		"\x1b[8mhidden\x1b[0m shown":                          "hidden shown",
		"progress 10%\rprogress 99%\ndone\r\n":                "progress 99%\ndone\n",
		"a\x1bPtmux;\x1b\x1b]52;c;eA==\x07\x1b\\b":            "ab",
		"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\": "link",
		"bell\x07 del\x7f c1\u009b31m":                        "bell del c131m",
		"stray \x9b byte":                                     "stray \uFFFD byte",
	}
	for in, want := range cases {
		if got := Clean(in); got != want {
			t.Errorf("Clean(%q) = %q, want %q", in, got, want)
		}
	}
}
