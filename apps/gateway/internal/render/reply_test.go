package render

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

const topic = "telegram--1001-topic-7"

func opts() Options {
	return Options{MiniAppBase: miniApp, ThreadID: "telegram--1001-topic-7", TopicThreadID: topic}
}

// file is a sendable reply_parts entry with the given send_as.
func file(path, sendAs string) File {
	m, err := MethodFromSendAs(sendAs)
	if err != nil {
		panic(err)
	}
	return File{Path: path, ArtifactKey: "0123456789abcdef", Linkable: true, HTML: strings.HasSuffix(path, ".html"), Size: 2048, Method: m}
}

func openURL() string {
	url, _ := ArtifactLink(miniApp, topic, "0123456789abcdef")
	return url
}

func threadLinked(text string) string {
	return AppendThreadLink(miniApp, "telegram--1001-topic-7", text)
}

// sent is one call the fake sender received.
type sent struct {
	text    string // message text or caption
	path    string
	method  Method
	caption bool
	rows    [][]string
}

type fakeSender struct {
	calls []sent
	// fail returns an error for the n-th call (0-based), or nil.
	fail func(n int, s sent) error
}

func (f *fakeSender) record(s sent) (int64, error) {
	n := len(f.calls)
	f.calls = append(f.calls, s)
	if f.fail != nil {
		if err := f.fail(n, s); err != nil {
			return 0, err
		}
	}
	return int64(100 + n), nil
}

func (f *fakeSender) SendText(_ context.Context, html string, kb *telegram.InlineKeyboardMarkup) (int64, error) {
	return f.record(sent{text: html, rows: buttons(kb)})
}

func (f *fakeSender) SendFile(_ context.Context, fl File, m Method, caption *string, kb *telegram.InlineKeyboardMarkup) (int64, error) {
	s := sent{path: fl.Path, method: m, rows: buttons(kb), caption: caption != nil}
	if caption != nil {
		s.text = *caption
	}
	return f.record(s)
}

var (
	rejected = &telegram.APIError{Method: "sendPhoto", Code: 400, Description: "Bad Request: wrong file"}
	floodErr = &telegram.APIError{Method: "sendMessage", Code: 429, Description: "Too Many Requests: retry after 30"}
	unknown  = &telegram.TransportError{Method: "sendMessage", Stage: telegram.Ambiguous, Detail: "timeout"}
)

func statuses(r Result) string {
	var out []string
	for _, o := range r.Items {
		s := string(o.Status)
		if o.MessageID != nil {
			s += fmt.Sprintf("#%d", *o.MessageID)
		}
		if o.LinkOnly {
			s += "(link)"
		}
		out = append(out, s)
	}
	return strings.Join(out, ",")
}

func run(t *testing.T, r Reply, o Options, fail func(int, sent) error) (Plan, *fakeSender, Result) {
	t.Helper()
	p := PlanReply(r, o)
	f := &fakeSender{fail: fail}
	return p, f, Deliver(context.Background(), f, p)
}

func TestShortTextAndOnePhotoAreOneCaptionedMessage(t *testing.T) {
	p, f, res := run(t, Reply{HTML: "<b>Done</b>", Followups: []string{"Next"}, Files: []File{file("/a/chart.png", "photo")}}, opts(), nil)
	if len(f.calls) != 1 || !f.calls[0].caption || f.calls[0].method != MethodPhoto || f.calls[0].text != threadLinked("<b>Done</b>") {
		t.Fatalf("%+v", f.calls)
	}
	eqRows(t, p.Steps[0].Keyboard, [][]string{{OpenArtifactLabel + "|" + openURL()}, {"Next|fu:0"}, {"✕ Dismiss|fu:x"}})
	if statuses(res) != "sent#100,sent#100" || res.FollowupsMessageID == nil || *res.FollowupsMessageID != 100 {
		t.Fatalf("%s", statuses(res))
	}
}

func TestShortTextAndOneVideoAreOneCaptionedMessage(t *testing.T) {
	p, f, res := run(t, Reply{HTML: "<b>Done</b>", Followups: []string{"Next"}, Files: []File{file("/a/clip.mp4", "video")}}, opts(), nil)
	if len(f.calls) != 1 || !f.calls[0].caption || f.calls[0].method != MethodVideo || f.calls[0].text != threadLinked("<b>Done</b>") {
		t.Fatalf("%+v", f.calls)
	}
	eqRows(t, p.Steps[0].Keyboard, [][]string{{OpenArtifactLabel + "|" + openURL()}, {"Next|fu:0"}, {"✕ Dismiss|fu:x"}})
	if statuses(res) != "sent#100,sent#100" || res.FollowupsMessageID == nil || *res.FollowupsMessageID != 100 {
		t.Fatalf("%s", statuses(res))
	}
}

func TestDocumentNeverTakesTheCaption(t *testing.T) {
	_, f, res := run(t, Reply{HTML: "Report attached", Followups: []string{"Next"}, Files: []File{file("/a/report.pdf", "document")}}, opts(), nil)
	if len(f.calls) != 2 || f.calls[0].path != "" || f.calls[1].path != "/a/report.pdf" || f.calls[1].caption {
		t.Fatalf("%+v", f.calls)
	}
	if fmt.Sprint(f.calls[0].rows) != "[[Next|fu:0] [✕ Dismiss|fu:x]]" || fmt.Sprint(f.calls[1].rows) != fmt.Sprintf("[[%s|%s]]", OpenArtifactLabel, openURL()) {
		t.Fatalf("follow-ups on the text, the file action on the file: %+v", f.calls)
	}
	if statuses(res) != "sent#100,sent#101" || *res.FollowupsMessageID != 100 {
		t.Fatal(statuses(res))
	}
}

func TestTextOverTheCaptionLimitGoesFirst(t *testing.T) {
	text := strings.Repeat("a", MaxCaptionLen)
	_, f, _ := run(t, Reply{HTML: text, Files: []File{file("/a/chart.png", "photo")}}, opts(), nil)
	if len(f.calls) != 2 || f.calls[0].text != threadLinked(text) || f.calls[1].caption {
		t.Fatalf("thread link pushes it over the caption limit: %+v", f.calls)
	}
}

func TestHTMLWithALinkIsACardOnlyWithTheMiniApp(t *testing.T) {
	_, f, res := run(t, Reply{HTML: "See it", Files: []File{file("/a/page.html", "document"), file("/a/notes.md", "document")}}, opts(), nil)
	if len(f.calls) != 3 || f.calls[1].path != "" || !strings.HasPrefix(f.calls[1].text, "📄 <b>page.html</b>\n2.0 KB") {
		t.Fatalf("%+v", f.calls)
	}
	if fmt.Sprint(f.calls[1].rows) != fmt.Sprintf("[[%s|%s %s|sf:0123456789abcdef]]", OpenArtifactLabel, openURL(), SendFileLabel) {
		t.Fatalf("%v", f.calls[1].rows)
	}
	if f.calls[2].path != "/a/notes.md" || statuses(res) != "sent#100,sent#101(link),sent#102" {
		t.Fatalf("only HTML becomes a card: %s", statuses(res))
	}

	_, f, res = run(t, Reply{HTML: "See it", Files: []File{file("/a/page.html", "document")}}, Options{ThreadID: "t"}, nil)
	if len(f.calls) != 2 || f.calls[0].text != "See it" || f.calls[1].path != "/a/page.html" || f.calls[1].rows != nil {
		t.Fatalf("no Mini App: uploaded, no buttons, no thread link: %+v", f.calls)
	}
	if statuses(res) != "sent#100,sent#101" {
		t.Fatal(statuses(res))
	}
}

func TestUnlinkableFileHasNoButton(t *testing.T) {
	f := file("/a/x/../report.pdf", "document")
	f.Linkable = false
	_, s, _ := run(t, Reply{Files: []File{f}}, opts(), nil)
	if len(s.calls) != 1 || s.calls[0].rows != nil {
		t.Fatalf("%+v", s.calls)
	}
}

func TestFilesWithoutTextAndSeparateFollowups(t *testing.T) {
	p, f, res := run(t, Reply{Followups: []string{"Next"}, Files: []File{file("/a/a.pdf", "document"), file("/a/b.png", "photo")}}, opts(), nil)
	if !p.SeparateFollowups || len(f.calls) != 2 || f.calls[1].method != MethodPhoto || f.calls[1].caption {
		t.Fatalf("%+v %+v", p, f.calls)
	}
	if res.FollowupsMessageID != nil || statuses(res) != "sent#100,sent#101" {
		t.Fatal(statuses(res))
	}
	p = PlanReply(Reply{HTML: "  ", Followups: []string{"Next"}}, opts())
	if len(p.Items) != 0 || len(p.Steps) != 0 || !p.SeparateFollowups {
		t.Fatalf("nothing to deliver, follow-ups alone: %+v", p)
	}
}

func TestSendAsDecidesNotTheExtension(t *testing.T) {
	_, s, _ := run(t, Reply{HTML: "Hi", Files: []File{file("/a/clip.png", "document")}}, opts(), nil)
	if len(s.calls) != 2 || s.calls[1].method != MethodDocument || s.calls[1].caption {
		t.Fatalf("a document is never captioned: %+v", s.calls)
	}
	_, s, _ = run(t, Reply{HTML: "Hi", Files: []File{file("/a/clip.bin", "audio")}}, opts(), nil)
	if len(s.calls) != 1 || s.calls[0].method != MethodAudio || !s.calls[0].caption {
		t.Fatalf("%+v", s.calls)
	}
}

func TestFileWithoutAMethodFailsInsteadOfGoingOutAsADocument(t *testing.T) {
	unset := file("/a/chart.png", "photo")
	unset.Method = MethodUnset
	bogus := file("/a/b.pdf", "document")
	bogus.Method = Method(99)
	_, s, res := run(t, Reply{HTML: "Hi", Files: []File{unset, bogus, file("/a/c.pdf", "document")}}, opts(), nil)
	if len(s.calls) != 2 || s.calls[0].caption || s.calls[1].path != "/a/c.pdf" {
		t.Fatalf("only the text and the valid file are sent: %+v", s.calls)
	}
	if statuses(res) != "sent#100,failed,failed,sent#101" || res.Items[1].Error != NoSendMethod || res.Items[2].Error != NoSendMethod {
		t.Fatal(statuses(res))
	}
	gone := file("/nope/a.png", "photo")
	gone.Method, gone.Missing = MethodUnset, true
	_, _, res = run(t, Reply{Files: []File{gone}}, opts(), nil)
	if res.Items[0].Error != FileNotFound {
		t.Fatalf("Missing wins: %q", res.Items[0].Error)
	}
}

func TestMethodFromSendAs(t *testing.T) {
	for in, want := range map[string]Method{"document": MethodDocument, "photo": MethodPhoto, "voice": MethodVoice, "audio": MethodAudio, "video": MethodVideo} {
		if got, err := MethodFromSendAs(in); err != nil || got != want {
			t.Errorf("MethodFromSendAs(%q) = %v %v", in, got, err)
		}
	}
	for _, in := range []string{"", "Video", "Photo"} {
		if got, err := MethodFromSendAs(in); err == nil || got != MethodUnset {
			t.Errorf("MethodFromSendAs(%q) = %v, want an error", in, got)
		}
	}
}

func TestMissingFileFailsWithoutASendAndTextStillGoes(t *testing.T) {
	missing := file("/a/gone.png", "photo")
	missing.Missing = true
	_, f, res := run(t, Reply{HTML: "Here", Files: []File{missing}}, opts(), nil)
	if len(f.calls) != 1 || f.calls[0].caption {
		t.Fatalf("a missing file is never captioned: %+v", f.calls)
	}
	if statuses(res) != "sent#100,failed" || res.Items[1].Error != FileNotFound {
		t.Fatal(statuses(res))
	}
}

func TestLongTextIsSplitWithFollowupsOnTheLastPart(t *testing.T) {
	text := strings.Repeat("word ", 2000)
	p, f, res := run(t, Reply{HTML: text, Followups: []string{"Next"}, Files: []File{file("/a/r.pdf", "document")}}, opts(), nil)
	if len(p.Items) != 4 || p.Items[0].Part != 1 || p.Items[2].Total != 3 || p.Items[3].Kind != ItemFile {
		t.Fatalf("%+v", p.Items)
	}
	if len(f.calls) != 4 || f.calls[0].rows != nil || f.calls[2].rows == nil || !strings.HasSuffix(f.calls[2].text, "↗ Open thread</a>") {
		t.Fatalf("%+v", f.calls)
	}
	if *res.FollowupsMessageID != 102 {
		t.Fatal("follow-ups ride on the last part")
	}
}

func TestFailedMiddlePartDoesNotStopTheRest(t *testing.T) {
	text := strings.Repeat("word ", 2000)
	_, f, res := run(t, Reply{HTML: text, Files: []File{file("/a/r.pdf", "document")}}, opts(), func(n int, _ sent) error {
		if n == 1 {
			return rejected
		}
		return nil
	})
	if len(f.calls) != 4 || statuses(res) != "sent#100,failed,sent#102,sent#103" {
		t.Fatal(statuses(res))
	}
	_, _, res = run(t, Reply{HTML: text}, opts(), func(n int, _ sent) error {
		if n == 0 {
			return unknown
		}
		return nil
	})
	if statuses(res) != "unknown,sent#101,sent#102" {
		t.Fatalf("ambiguous is unknown: %s", statuses(res))
	}
}

func TestFloodRefusalStopsTheRemainingItems(t *testing.T) {
	_, f, res := run(t, Reply{HTML: "Text", Files: []File{file("/a/a.pdf", "document"), file("/a/b.pdf", "document")}}, opts(), func(n int, _ sent) error {
		if n == 1 {
			return floodErr
		}
		return nil
	})
	if len(f.calls) != 2 || statuses(res) != "sent#100,failed,failed" {
		t.Fatal(statuses(res))
	}
	if !strings.HasPrefix(res.Items[2].Error, "not attempted after a flood refusal: ") {
		t.Fatal(res.Items[2].Error)
	}
}

func TestRefusedCaptionedFileStillDeliversTheText(t *testing.T) {
	_, f, res := run(t, Reply{HTML: "Hi", Followups: []string{"Next"}, Files: []File{file("/a/v.ogg", "voice")}}, opts(), func(n int, _ sent) error {
		if n == 0 {
			return rejected
		}
		return nil
	})
	if len(f.calls) != 2 || f.calls[1].path != "" || fmt.Sprint(f.calls[1].rows) != "[[Next|fu:0] [✕ Dismiss|fu:x]]" {
		t.Fatalf("%+v", f.calls)
	}
	if statuses(res) != "sent#101,failed" || *res.FollowupsMessageID != 101 {
		t.Fatal(statuses(res))
	}
	for _, c := range []struct {
		err  error
		want string
	}{{unknown, "unknown,unknown"}, {floodErr, "failed,failed"}} {
		_, f, res := run(t, Reply{HTML: "Hi", Files: []File{file("/a/v.ogg", "voice")}}, opts(), func(int, sent) error { return c.err })
		if len(f.calls) != 1 || statuses(res) != c.want {
			t.Fatalf("%v: never resent: %d calls, %s", c.err, len(f.calls), statuses(res))
		}
	}
}

func TestWorkerLinksComeBeforeTheThreadLink(t *testing.T) {
	o := opts()
	o.Workers = []WorkerLink{{Title: "w", URL: "https://t.me/c/1/2"}}
	p := PlanReply(Reply{HTML: "ok"}, o)
	want := threadLinked("ok\n\n🛠 <a href=\"https://t.me/c/1/2\">w</a>")
	if p.Steps[0].Text != want {
		t.Fatalf("%q", p.Steps[0].Text)
	}
}
