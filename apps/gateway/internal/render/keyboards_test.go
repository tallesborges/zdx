package render

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

const miniApp = "https://t.me/example_bot/app"

func buttons(kb *telegram.InlineKeyboardMarkup) [][]string {
	if kb == nil {
		return nil
	}
	var rows [][]string
	for _, row := range kb.InlineKeyboard {
		var r []string
		for _, b := range row {
			switch {
			case b.CallbackData != nil:
				r = append(r, b.Text+"|"+*b.CallbackData)
			case b.URL != nil:
				r = append(r, b.Text+"|"+*b.URL)
			}
		}
		rows = append(rows, r)
	}
	return rows
}

func eqRows(t *testing.T, got *telegram.InlineKeyboardMarkup, want [][]string) {
	t.Helper()
	g, _ := json.Marshal(buttons(got))
	w, _ := json.Marshal(want)
	if string(g) != string(w) {
		t.Fatalf("keyboard\n got %s\nwant %s", g, w)
	}
}

func TestFollowupKeyboard(t *testing.T) {
	if FollowupKeyboard(nil) != nil {
		t.Fatal("no items, no keyboard")
	}
	long := strings.Repeat("x", 80)
	kb := FollowupKeyboard([]string{"Run **the** `tests`", "__Commit__ snake_case", long})
	eqRows(t, kb, [][]string{
		{"Run the tests|fu:0"}, {"Commit snake_case|fu:1"}, {strings.Repeat("x", 63) + "…|fu:2"}, {"✕ Dismiss|fu:x"},
	})
	kept := [][]telegram.InlineKeyboardButton{{telegram.URLButton(OpenArtifactLabel, "https://e/a")}}
	eqRows(t, ChosenFollowupKeyboard(kept, "Run `it`"), [][]string{{"↗ Open artifact|https://e/a"}, {"▶️ Run it|fu:-"}})

	for data, want := range map[string]FollowupTap{"fu:2": {Index: 2}, "fu:x": {Dismiss: true, Index: -1}, "fu:-": {Index: -1}} {
		if got, ok := ParseFollowupData(data); !ok || got != want {
			t.Errorf("ParseFollowupData(%q) = %+v %v", data, got, ok)
		}
	}
	if _, ok := ParseFollowupData("sf:1"); ok {
		t.Fatal("other prefixes are not follow-ups")
	}
}

func TestLinkRowsKeepOnlyURLRows(t *testing.T) {
	markup := json.RawMessage(`{"inline_keyboard":[
		[{"text":"↗ Open artifact","url":"https://e/a"}],
		[{"text":"Run","callback_data":"fu:0"}],
		[{"text":"a","url":"https://e/1"},{"text":"b","callback_data":"x"}],
		[{"text":"✕ Dismiss","callback_data":"fu:x"}]]}`)
	eqRows(t, &telegram.InlineKeyboardMarkup{InlineKeyboard: LinkRows(markup)}, [][]string{{"↗ Open artifact|https://e/a"}})
	if LinkRows(nil) != nil || LinkRows(json.RawMessage(`{`)) != nil {
		t.Fatal("no markup, no rows")
	}
}

func TestArtifactLink(t *testing.T) {
	key := "0123456789abcdef"
	url, ok := ArtifactLink(miniApp, "telegram--1001234567890-topic-1234567", key)
	if !ok || url != miniApp+"?startapp=a_"+key+"_telegram--1001234567890-topic-1234567" {
		t.Fatalf("%q %v", url, ok)
	}
	if _, ok := ArtifactLink(miniApp, strings.Repeat("9", 46), key); ok {
		t.Fatal("over 64 characters")
	}
	if _, ok := ArtifactLink(miniApp, "thread with space", key); ok {
		t.Fatal("outside the alphabet")
	}
}

// Cases from response.rs tests.
func TestThreadAndWorkerLinks(t *testing.T) {
	got := AppendThreadLink("https://t.me/example_bot/app", "telegram--1001234567890-topic-17771", "done")
	want := "done\n\n<a href=\"https://t.me/example_bot/app?startapp=telegram--1001234567890-topic-17771\">↗ Open thread</a>"
	if got != want {
		t.Fatalf("%q", got)
	}
	if AppendThreadLink("", "t", "done") != "done" {
		t.Fatal("no Mini App, unchanged")
	}
	links := []WorkerLink{{Title: "dub · fix <PR> comments", URL: "https://t.me/c/1/2"}, {ID: "0123456789", URL: "https://t.me/c/1/3"}}
	got = AppendWorkerLinks(links, "sent")
	want = "sent\n\n🛠 <a href=\"https://t.me/c/1/2\">dub · fix &lt;PR&gt; comments</a>\n🛠 <a href=\"https://t.me/c/1/3\">01234567</a>"
	if got != want {
		t.Fatalf("%q", got)
	}
	if AppendWorkerLinks(nil, "sent") != "sent" {
		t.Fatal("no workers, unchanged")
	}
	for _, c := range []struct{ mirror, base, want string }{
		{"https://t.me/c/1/9", miniApp, "https://t.me/c/1/9"},
		{"", miniApp, miniApp + "?startapp=w1"},
		{"", "", ""},
	} {
		if got, ok := WorkerURL(c.mirror, c.base, "w1"); got != c.want || ok != (c.want != "") {
			t.Errorf("WorkerURL(%q, %q) = %q %v", c.mirror, c.base, got, ok)
		}
	}
}

func TestCardText(t *testing.T) {
	want := "📄 <b>a&lt;b&gt;.html</b>\n1.5 KB · HTML · not uploaded\n<i>Open artifact and Send file use the file as it is now on the Mac.</i>"
	if got := CardText("a<b>.html", 1536); got != want {
		t.Fatalf("%q", got)
	}
	for n, want := range map[int64]string{0: "0 B", 1023: "1023 B", 1024: "1.0 KB", 150 * 1024: "150 KB", 5 << 20: "5.0 MB", 3 << 30: "3.0 GB"} {
		if got := formatBytes(n); got != want {
			t.Errorf("formatBytes(%d) = %q, want %q", n, got, want)
		}
	}
	eqRows(t, CardKeyboard("https://e/a", "0123456789abcdef"), [][]string{{"↗ Open artifact|https://e/a", "⬇ Send file|sf:0123456789abcdef"}})
}
