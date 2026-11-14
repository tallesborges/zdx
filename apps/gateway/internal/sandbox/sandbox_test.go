package sandbox

import (
	"errors"
	"os"
	"strings"
	"testing"
)

func TestCheckHomeAndEnv(t *testing.T) {
	t.Setenv(Env, "/sb/x")
	for home, ok := range map[string]bool{
		"/sb/x/h/.zdx": true, "/sb/x": true, "/sb/xy": false, "/sb": false, "relative": false, "/sb/x/../y": false,
	} {
		if err := CheckHome(home); (err == nil) != ok || (err != nil && !errors.Is(err, ErrOutside)) {
			t.Errorf("CheckHome(%q) = %v", home, err)
		}
	}
	cases := []struct {
		env []string
		ok  bool
	}{
		{[]string{"ZDX_HOME=/sb/x/z", "HOME=/real"}, true},
		{[]string{"HOME=/sb/x/h"}, true},
		{[]string{"ZDX_HOME=/real/.zdx", "HOME=/sb/x/h"}, false},
		{[]string{"HOME=/real"}, false},
		{[]string{"PATH=/bin"}, false},
	}
	for _, c := range cases {
		if err := CheckEnv(c.env); (err == nil) != c.ok {
			t.Errorf("CheckEnv(%v) = %v", c.env, err)
		}
	}
	t.Setenv(Env, "")
	if CheckHome("/anything") != nil || CheckEnv([]string{"HOME=/real"}) != nil {
		t.Fatal("without a sandbox nothing is refused")
	}
}

// Enter drops every inherited ZDX_ variable except the real-Rust test
// switches; other variables (fake provider settings a test passes on
// explicitly) are left to the child allowlists.
func TestEnterDropsInheritedZdxVariables(t *testing.T) {
	for _, k := range []string{"HOME", "ZDX_HOME", "TMPDIR", Env, "PATH"} {
		t.Setenv(k, os.Getenv(k))
	}
	inherited := map[string]string{
		"ZDX_BIN": "/real/zdx", "ZDX_TELEGRAM_BOT_TOKEN": "1:SECRET", "ZDX_SERVICE_SUPERVISOR": "launchd", "ZDX_THREAD_ID": "t",
		RustBinaryEnv: "/tmp/zdx-snapshot", RustStrictEnv: "1", "ANTHROPIC_BASE_URL": "http://127.0.0.1:1",
	}
	for k, v := range inherited {
		t.Setenv(k, v)
	}
	check, err := Enter(false)
	if err != nil {
		t.Fatal(err)
	}
	defer check()
	for _, k := range []string{"ZDX_BIN", "ZDX_TELEGRAM_BOT_TOKEN", "ZDX_SERVICE_SUPERVISOR", "ZDX_THREAD_ID"} {
		if v, ok := os.LookupEnv(k); ok {
			t.Errorf("%s=%s survived", k, v)
		}
	}
	for _, k := range []string{RustBinaryEnv, RustStrictEnv, "ANTHROPIC_BASE_URL"} {
		if os.Getenv(k) != inherited[k] {
			t.Errorf("%s must stay", k)
		}
	}
	if !strings.HasPrefix(os.Getenv("ZDX_HOME"), os.Getenv(Env)) {
		t.Error("ZDX_HOME must move into the sandbox")
	}
}
