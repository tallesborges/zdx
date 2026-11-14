package gateway

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// helperService answers the helper and catalog ops with canned bodies and
// records each request.
func helperService(t *testing.T, caps []string, answer func(req map[string]any) string) (*engine.Client, func() []map[string]any) {
	dir, _ := os.MkdirTemp("", "gwh")
	t.Cleanup(func() { os.RemoveAll(dir) })
	sock := filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	var mu sync.Mutex
	var reqs []map[string]any
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			line, _ := bufio.NewReader(conn).ReadBytes('\n')
			var req map[string]any
			json.Unmarshal(line, &req)
			if req["op"] == "status" {
				b, _ := json.Marshal(map[string]any{"reply": "status", "report": map[string]any{"protocol": 2, "capabilities": caps, "settled": true, "threads": []any{}}})
				conn.Write(append(b, '\n'))
			} else {
				mu.Lock()
				reqs = append(reqs, req)
				mu.Unlock()
				conn.Write([]byte(answer(req) + "\n"))
			}
			conn.Close()
		}
	}()
	return engine.NewClient(sock), func() []map[string]any {
		mu.Lock()
		defer mu.Unlock()
		return append([]map[string]any(nil), reqs...)
	}
}

// The adapters send Rust's wire shapes (chat root, scope names, optional
// thread ids) and map the replies field by field.
func TestServiceHelpersWireShapes(t *testing.T) {
	caps := []string{engine.CapGenerate, engine.CapBtwSeed, engine.CapTopicStart, engine.CapCatalog, engine.CapSetChatModel, engine.CapUpdateThread}
	eng, sent := helperService(t, caps, func(req map[string]any) string {
		switch req["op"] {
		case "generate":
			return `{"reply":"generated","text":"gen:` + req["kind"].(string) + `"}`
		case "btw_seed":
			return `{"reply":"text","text":"seed"}`
		case "topic_start":
			if req["pick"] != nil {
				return `{"reply":"topic_start","selection":"picked","label":"L"}`
			}
			return `{"reply":"topic_start","selection":null,"label":"default label"}`
		case "set_chat_model":
			return `{"reply":"chat_model","selection":"anthropic:m@high"}`
		case "update_thread":
			return `{"reply":"thread_updated","thread_id":"t","model_label":"label"}`
		case "catalog":
			switch req["kind"] {
			case "picker":
				return `{"reply":"catalog","body":{"models":[{"id":"p:m","name":"p:m","specs":{"off":"p:m@off"}}],"provider_labels":{"p":"Prov"},"modes":[{"name":"fast","ref":"mode:fast"}],"label":"L","default_label":"D","overridden":true,"override":"p:m","model":"p:m","thinking":"off"}}`
			case "launcher":
				return `{"reply":"catalog","body":{"modes":[{"name":"fast","ref":"mode:fast","primary":"p:m@low","thinking":"low","description":"d"}],"default_mode":"mode:fast","default_model":"p:m","default_thinking":"low"}}`
			case "resumable":
				return `{"reply":"catalog","body":{"threads":[{"id":"a","title":"A","modified":"2026-01-02T03:04:05Z"},{"id":"b","title":"B"}]}}`
			case "resume_source":
				return `{"reply":"catalog","body":{"title":null}}`
			case "thread_status":
				return `{"reply":"catalog","body":{"orchestrator":true,"model":"p:m@low","model_source":"topic","thinking":"low","thinking_source":"default","thread_id":"t","root":"/r","context_limit":100,"context_tokens":5,"usage":{"input":1,"output":2,"cache_read":3,"cache_write":4},"pricing":{"kind":"priced","cost":0.5,"savings":0.1,"input":1,"output":2,"cache_read":0.1,"cache_write":1.25},"workers":[{"thread_id":"w","status":"running"}],"parent_orchestrator":"o"}}`
			case "custom_commands":
				return `{"reply":"catalog","body":{"commands":[{"name":"ship","source":"project","content":"go"}]}}`
			}
		}
		return `{"reply":"error","message":"unexpected"}`
	})
	s := serviceHelpers{engine: eng, root: func(chat int64) string {
		if chat == -100 {
			return "/chat-root"
		}
		return "/bot-root"
	}}
	ctx := context.Background()
	must := func(err error) {
		t.Helper()
		if err != nil {
			t.Fatal(err)
		}
	}
	if out, err := s.TLDR(ctx, -100, "t"); err != nil || out != "gen:tldr" {
		t.Fatal(out, err)
	}
	_, err := s.Handoff(ctx, -100, "t", "goal")
	must(err)
	_, err = s.PromptBuilder(ctx, -100, "", "intent")
	must(err)
	start, err := s.StartModel(ctx, -100, "src", "orchestrator")
	if err != nil || start.Pin != nil || start.Label != "default label" {
		t.Fatalf("start %+v %v", start, err)
	}
	if sel, err := s.ResolveStagingPick(ctx, -100, "src", "", "p:m"); err != nil || sel != "picked" {
		t.Fatal(sel, err)
	}
	_, err = s.SideQuestionSeed(ctx, "src", "q?")
	must(err)
	p, err := s.Picker(ctx, commands.PickerQuery{Chat: -100, Scope: commands.ScopeNewOrchestrator})
	if err != nil || len(p.Models) != 1 || p.Models[0].Specs["off"] != "p:m@off" || p.ProviderLabels["p"] != "Prov" || p.Modes[0].Ref != "mode:fast" || !p.Overridden || p.Override != "p:m" {
		t.Fatalf("picker %+v %v", p, err)
	}
	_, err = s.Picker(ctx, commands.PickerQuery{Chat: -100, Scope: commands.ScopeNewThread})
	must(err)
	if label, err := s.SetTopicModel(ctx, -100, "t", "p:m"); err != nil || label != "label" {
		t.Fatal(label, err)
	}
	must(s.SetChatModel(ctx, -100, "p:m@high"))
	v, err := s.LauncherView(ctx, -100, commands.RoleAgent)
	if err != nil || v.DefaultMode != "mode:fast" || v.Modes[0].Primary != "p:m@low" {
		t.Fatalf("launcher %+v %v", v, err)
	}
	r, err := s.Resumable(ctx, -100)
	if err != nil || len(r) != 2 || r[0].Modified == nil || r[0].Modified.Year() != 2026 || r[1].Modified != nil {
		t.Fatalf("resumable %+v %v", r, err)
	}
	if title, err := s.ResumeSource(ctx, "x"); err != nil || title != "" {
		t.Fatal(title, err)
	}
	st, err := s.ThreadStatus(ctx, -100, "t")
	if err != nil || !st.Orchestrator || st.Pricing.Kind != "priced" || st.Pricing.CacheWrite != 1.25 || st.Usage.CacheWrite != 4 || st.Workers[0].Status != "running" || st.ParentOrchestrator != "o" || st.Profile != "" {
		t.Fatalf("status %+v %v", st, err)
	}
	c, err := s.CustomCommands(ctx, -100, "", []string{"model"})
	if err != nil || len(c) != 1 || c[0].Description != "" {
		t.Fatalf("commands %+v %v", c, err)
	}

	reqs := sent()
	want := []string{
		`{"kind":"tldr","op":"generate","root":"/chat-root","thread_id":"t"}`,
		`{"goal":"goal","kind":"handoff","op":"generate","root":"/chat-root","thread_id":"t"}`,
		`{"kind":"prompt_builder","op":"generate","root":"/chat-root","text":"intent"}`,
		`{"op":"topic_start","profile":"orchestrator","root":"/chat-root","source_thread_id":"src"}`,
		`{"op":"topic_start","pick":"p:m","root":"/chat-root","source_thread_id":"src"}`,
		`{"op":"btw_seed","question":"q?","thread_id":"src"}`,
		`{"kind":"picker","op":"catalog","root":"/chat-root","scope":"new_orchestrator"}`,
		`{"kind":"picker","op":"catalog","root":"/chat-root","scope":"new_thread"}`,
		`{"config_root":"/chat-root","model":"p:m","op":"update_thread","thread_id":"t"}`,
		`{"model":"p:m@high","op":"set_chat_model","root":"/chat-root"}`,
		`{"kind":"launcher","op":"catalog","role":"agent","root":"/chat-root"}`,
		`{"kind":"resumable","op":"catalog","root":"/chat-root"}`,
		`{"kind":"resume_source","op":"catalog","thread_id":"x"}`,
		`{"kind":"thread_status","op":"catalog","root":"/chat-root","thread_id":"t"}`,
		`{"kind":"custom_commands","native":["model"],"op":"catalog","root":"/chat-root"}`,
	}
	if len(reqs) != len(want) {
		t.Fatalf("%d requests: %v", len(reqs), reqs)
	}
	for i, r := range reqs {
		b, _ := json.Marshal(r)
		if string(b) != want[i] {
			t.Errorf("request %d:\n got %s\nwant %s", i, b, want[i])
		}
	}
}

// A new topic's model and profile are checked against its chat's root;
// no root is stored for them.
func TestThreadSetupSendsTheChatRootWithAModel(t *testing.T) {
	eng, sent := helperService(t, []string{engine.CapUpdateThread}, func(map[string]any) string {
		return `{"reply":"thread_updated","thread_id":"t"}`
	})
	s := threadSetup{engine: eng, root: func(int64) string { return "/chat-root" }}
	model := "mode:fast"
	if err := s.SetupThread(context.Background(), commands.ThreadSetup{ThreadID: "telegram--100-topic-9", Model: &model, PendingTitle: true}); err != nil {
		t.Fatal(err)
	}
	if err := s.SetupThread(context.Background(), commands.ThreadSetup{ThreadID: "telegram--100-topic-10", HandoffFrom: "src"}); err != nil {
		t.Fatal(err)
	}
	if err := s.SetupThread(context.Background(), commands.ThreadSetup{ThreadID: "telegram--100-topic-11", Profile: "orchestrator"}); err != nil {
		t.Fatal(err)
	}
	reqs := sent()
	if reqs[0]["config_root"] != "/chat-root" || reqs[0]["model"] != "mode:fast" || reqs[0]["pending_topic_title"] != true {
		t.Fatalf("with a model: %v", reqs[0])
	}
	if _, ok := reqs[1]["config_root"]; ok || !strings.Contains(reqs[1]["handoff_from"].(string), "src") || reqs[1]["root"] != nil {
		t.Fatalf("without a model: %v", reqs[1])
	}
	if _, pinned := reqs[2]["root"]; reqs[2]["persistent_profile"] != "orchestrator" || reqs[2]["config_root"] != "/chat-root" || pinned {
		t.Fatalf("a profile is checked against the chat root, which is not stored: %v", reqs[2])
	}
}
