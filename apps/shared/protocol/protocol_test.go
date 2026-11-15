package protocol

import (
	"encoding/json"
	"errors"
	"testing"
	"time"
)

func TestEncodeLineTagsOpFirstWithoutHTMLEscaping(t *testing.T) {
	line, err := EncodeLine("status", Status{})
	if err != nil {
		t.Fatal(err)
	}
	if string(line) != `{"op":"status"}` {
		t.Fatalf("empty body: %s", line)
	}
	id := "<a&b>"
	line, _ = EncodeLine("status", Status{ThreadID: &id})
	if string(line) != `{"op":"status","thread_id":"<a&b>"}` {
		t.Fatalf("got %s", line)
	}
	if _, err := EncodeLine("x", []int{1}); err == nil {
		t.Fatal("a non-object body must fail")
	}
}

func TestViewEncodeSendsOnlySetFields(t *testing.T) {
	thread, days := "t1", uint32(7)
	line, err := View{Op: ViewThreadView, ThreadID: &thread}.Encode()
	if err != nil || string(line) != `{"op":"thread_view","thread_id":"t1"}` {
		t.Fatalf("%s %v", line, err)
	}
	line, _ = View{Op: ViewUsageReport, Days: &days}.Encode()
	if string(line) != `{"op":"usage_report","days":7}` {
		t.Fatalf("%s", line)
	}
	line, _ = View{Op: ViewMonitorSnapshot}.Encode()
	if string(line) != `{"op":"monitor_snapshot"}` {
		t.Fatalf("%s", line)
	}
}

func TestViewCapabilityAndBudget(t *testing.T) {
	cases := map[string]string{
		ViewThreadsList: CapThreadViews, ViewThreadWorkers: CapThreadViews,
		ViewArtifactCanonical: CapArtifactViews, ViewMonitorSnapshot: CapMonitorViews,
		ViewUsageReport: CapMonitorViews, ViewThreadRoot: CapThreadRoot, "nope": "",
		// Retired: quotas are read with the quota package, and the Git
		// views run in the gateway with the gitview package.
		"subscription_quotas": "", "git_status": "", "git_scope": "", "git_diff": "",
	}
	for op, want := range cases {
		if got := ViewCapability(op); got != want {
			t.Errorf("%s: %q, want %q", op, got, want)
		}
	}
	if ViewBudget("subscription_quotas", time.Second) != time.Second ||
		ViewBudget(ViewArtifactCanonical, time.Second) != CanonicalTimeout ||
		ViewBudget(ViewThreadsList, time.Second) != time.Second {
		t.Fatal("view budgets")
	}
}

func TestDecodeViewReply(t *testing.T) {
	out, err := DecodeViewReply([]byte(`{"reply":"view","body":[1,2],"source_id":"s"}`), ViewThreadView)
	if err != nil || string(out.Body) != `[1,2]` || out.SourceID == nil || *out.SourceID != "s" {
		t.Fatalf("%+v %v", out, err)
	}
	_, err = DecodeViewReply([]byte(`{"reply":"error","message":"no such thread","code":"not_found"}`), ViewThreadView)
	var se *ServiceError
	if !errors.As(err, &se) || se.Code != "not_found" || se.Error() != "no such thread" {
		t.Fatalf("error reply: %v", err)
	}
	if _, err := DecodeViewReply([]byte(`{"reply":"view"}`), ViewThreadView); err == nil {
		t.Fatal("a view reply without a body must fail")
	}
	if _, err := DecodeViewReply([]byte(`{"reply":"ok"}`), ViewThreadView); err == nil {
		t.Fatal("another reply kind must fail")
	}
	if _, err := DecodeViewReply([]byte(`not json`), ViewThreadView); err == nil {
		t.Fatal("garbage must fail")
	}
}

func TestStatusReportCapabilitiesNeedProtocolTwo(t *testing.T) {
	report, err := DecodeStatusReply([]byte(`{"reply":"status","report":{"settled":true,"threads":[],"capabilities":["thread_views"]}}`))
	if err != nil {
		t.Fatal(err)
	}
	if report.Version() != 1 || report.Has(CapThreadViews) {
		t.Fatal("a report without protocol is v1 without capabilities")
	}
	report, _ = DecodeStatusReply([]byte(`{"reply":"status","report":{"protocol":2,"settled":true,"threads":[],"capabilities":["thread_views"]}}`))
	if report.Version() != 2 || !report.Has(CapThreadViews) || report.Has(CapThreadRoot) {
		t.Fatalf("v2: %+v", report)
	}
	var nilReport *StatusReport
	if nilReport.Version() != 1 {
		t.Fatal("nil report is v1")
	}
	if _, err := DecodeStatusReply([]byte(`{"reply":"error","message":"m"}`)); err == nil {
		t.Fatal("error reply")
	}
	if _, err := DecodeStatusReply([]byte(`{"reply":"status"}`)); err == nil {
		t.Fatal("status without report")
	}
}

func TestWorkerSnapshotKeepsUnknownFields(t *testing.T) {
	in := `{"thread_id":"w","owner_thread_id":"o","root":"/r","status":"running","queue_depth":2,"latest_final_text":null,"last_error":null,"mirror_url":null,"current_tool":"bash","future":1}`
	var w WorkerSnapshot
	if err := json.Unmarshal([]byte(in), &w); err != nil {
		t.Fatal(err)
	}
	if w.ThreadID != "w" || w.QueueDepth != 2 || *w.CurrentTool != "bash" {
		t.Fatalf("%+v", w)
	}
	out, _ := json.Marshal(w)
	if string(out) != in {
		t.Fatalf("pass-through changed: %s", out)
	}
}
