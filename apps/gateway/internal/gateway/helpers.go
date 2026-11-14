package gateway

import (
	"context"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// Adapters of the commands package over the Rust helper and catalog
// operations (contracts v9 and v10). Every request carries the chat's root
// (Settings.RootForChat, the same root the bot submits turns with); Rust
// derives everything else. Each operation checks its own capability.

// serviceHelpers implements commands.Generator, Seeds, Models,
// ChatDefaults, Launches, Statuses and CommandSource.
type serviceHelpers struct {
	engine *engine.Client
	root   func(chat int64) string
}

var (
	_ commands.Generator     = serviceHelpers{}
	_ commands.Seeds         = serviceHelpers{}
	_ commands.Models        = serviceHelpers{}
	_ commands.ChatDefaults  = serviceHelpers{}
	_ commands.Launches      = serviceHelpers{}
	_ commands.Statuses      = serviceHelpers{}
	_ commands.CommandSource = serviceHelpers{}
)

func optionalID(id string) *string {
	if id == "" {
		return nil
	}
	return &id
}

func (s serviceHelpers) TLDR(ctx context.Context, chat int64, threadID string) (string, error) {
	return s.engine.Generate(ctx, engine.Generate{Kind: engine.GenerateTLDR, Root: s.root(chat), ThreadID: &threadID})
}

func (s serviceHelpers) Handoff(ctx context.Context, chat int64, threadID, goal string) (string, error) {
	return s.engine.Generate(ctx, engine.Generate{Kind: engine.GenerateHandoff, Root: s.root(chat), ThreadID: &threadID, Goal: goal})
}

func (s serviceHelpers) PromptBuilder(ctx context.Context, chat int64, threadID, intent string) (string, error) {
	return s.engine.Generate(ctx, engine.Generate{Kind: engine.GeneratePromptBuilder, Root: s.root(chat), ThreadID: optionalID(threadID), Text: intent})
}

func (s serviceHelpers) StartModel(ctx context.Context, chat int64, source, profile string) (commands.StartModel, error) {
	start, err := s.engine.TopicStart(ctx, engine.TopicStart{Root: s.root(chat), SourceThreadID: source, Profile: profile})
	if err != nil {
		return commands.StartModel{}, err
	}
	return commands.StartModel{Pin: start.Selection, Label: start.Label}, nil
}

func (s serviceHelpers) SideQuestionSeed(ctx context.Context, source, question string) (string, error) {
	return s.engine.BtwSeed(ctx, source, question)
}

// pickerScopes are the wire names of the picker scopes.
var pickerScopes = map[commands.Scope]string{
	commands.ScopeGeneral:         "general",
	commands.ScopeTopic:           "topic",
	commands.ScopeNewThread:       "new_thread",
	commands.ScopeNewOrchestrator: "new_orchestrator",
	commands.ScopeStaging:         "staging",
}

func (s serviceHelpers) Picker(ctx context.Context, q commands.PickerQuery) (commands.Picker, error) {
	var body struct {
		ConfigError string `json:"config_error"`
		LaunchError string `json:"launch_error"`
		Models      []struct {
			ID    string            `json:"id"`
			Name  string            `json:"name"`
			Specs map[string]string `json:"specs"`
		} `json:"models"`
		ProviderLabels map[string]string `json:"provider_labels"`
		Modes          []struct {
			Name string `json:"name"`
			Ref  string `json:"ref"`
		} `json:"modes"`
		Label        string `json:"label"`
		DefaultLabel string `json:"default_label"`
		Overridden   bool   `json:"overridden"`
		Override     string `json:"override"`
		Model        string `json:"model"`
		Thinking     string `json:"thinking"`
	}
	req := engine.Catalog{Kind: engine.CatalogPicker, Root: s.root(q.Chat), Scope: pickerScopes[q.Scope], ThreadID: optionalID(q.ThreadID), Profile: q.Profile}
	if err := s.engine.Catalog(ctx, req, &body); err != nil {
		return commands.Picker{}, err
	}
	p := commands.Picker{
		ConfigError: body.ConfigError, LaunchError: body.LaunchError, ProviderLabels: body.ProviderLabels,
		Label: body.Label, DefaultLabel: body.DefaultLabel, Overridden: body.Overridden, Override: body.Override,
		Model: body.Model, Thinking: body.Thinking,
	}
	for _, m := range body.Models {
		p.Models = append(p.Models, commands.PickerModel{ID: m.ID, Name: m.Name, Specs: m.Specs})
	}
	for _, m := range body.Modes {
		p.Modes = append(p.Modes, commands.PickerMode{Name: m.Name, Ref: m.Ref})
	}
	return p, nil
}

func (s serviceHelpers) SetTopicModel(ctx context.Context, chat int64, threadID, model string) (string, error) {
	root := s.root(chat)
	updated, err := s.engine.UpdateThread(ctx, engine.UpdateThread{ThreadID: threadID, ConfigRoot: &root, Model: &model})
	if err != nil {
		return "", err
	}
	if updated.ModelLabel == nil {
		return model, nil
	}
	return *updated.ModelLabel, nil
}

func (s serviceHelpers) ResolveStagingPick(ctx context.Context, chat int64, source, profile, model string) (string, error) {
	start, err := s.engine.TopicStart(ctx, engine.TopicStart{Root: s.root(chat), SourceThreadID: source, Profile: profile, Pick: model})
	if err != nil {
		return "", err
	}
	if start.Selection == nil {
		return model, nil
	}
	return *start.Selection, nil
}

func (s serviceHelpers) SetChatModel(ctx context.Context, chat int64, model string) error {
	_, err := s.engine.SetChatModel(ctx, s.root(chat), model)
	return err
}

func (s serviceHelpers) LauncherView(ctx context.Context, chat int64, role string) (commands.LauncherView, error) {
	var body struct {
		ConfigError string `json:"config_error"`
		LaunchError string `json:"launch_error"`
		Modes       []struct {
			Name        string `json:"name"`
			Ref         string `json:"ref"`
			Primary     string `json:"primary"`
			Thinking    string `json:"thinking"`
			Description string `json:"description"`
		} `json:"modes"`
		DefaultMode     string `json:"default_mode"`
		DefaultModel    string `json:"default_model"`
		DefaultThinking string `json:"default_thinking"`
	}
	if err := s.engine.Catalog(ctx, engine.Catalog{Kind: engine.CatalogLauncher, Root: s.root(chat), Role: role}, &body); err != nil {
		return commands.LauncherView{}, err
	}
	v := commands.LauncherView{
		ConfigError: body.ConfigError, LaunchError: body.LaunchError, DefaultMode: body.DefaultMode,
		DefaultModel: body.DefaultModel, DefaultThinking: body.DefaultThinking,
	}
	for _, m := range body.Modes {
		v.Modes = append(v.Modes, commands.LaunchMode{Name: m.Name, Ref: m.Ref, Primary: m.Primary, Thinking: m.Thinking, Description: m.Description})
	}
	return v, nil
}

func (s serviceHelpers) Resumable(ctx context.Context, chat int64) ([]commands.ResumeThread, error) {
	var body struct {
		Threads []struct {
			ID       string  `json:"id"`
			Title    string  `json:"title"`
			Modified *string `json:"modified"`
		} `json:"threads"`
	}
	if err := s.engine.Catalog(ctx, engine.Catalog{Kind: engine.CatalogResumable, Root: s.root(chat)}, &body); err != nil {
		return nil, err
	}
	out := make([]commands.ResumeThread, 0, len(body.Threads))
	for _, t := range body.Threads {
		r := commands.ResumeThread{ID: t.ID, Title: t.Title}
		if t.Modified != nil {
			if at, err := time.Parse(time.RFC3339, *t.Modified); err == nil {
				r.Modified = &at
			}
		}
		out = append(out, r)
	}
	return out, nil
}

func (s serviceHelpers) ResumeSource(ctx context.Context, source string) (string, error) {
	var body struct {
		Title *string `json:"title"`
	}
	if err := s.engine.Catalog(ctx, engine.Catalog{Kind: engine.CatalogResumeSource, ThreadID: &source}, &body); err != nil {
		return "", err
	}
	if body.Title == nil {
		return "", nil
	}
	return *body.Title, nil
}

func (s serviceHelpers) ThreadStatus(ctx context.Context, chat int64, threadID string) (commands.ThreadStatus, error) {
	var body struct {
		ConfigError    string `json:"config_error"`
		Orchestrator   bool   `json:"orchestrator"`
		Refused        string `json:"refused"`
		Model          string `json:"model"`
		ModelSource    string `json:"model_source"`
		Mode           string `json:"mode"`
		Thinking       string `json:"thinking"`
		ThinkingSource string `json:"thinking_source"`
		ThreadID       string `json:"thread_id"`
		Root           string `json:"root"`
		Branch         string `json:"branch"`
		ContextLimit   uint64 `json:"context_limit"`
		ContextTokens  uint64 `json:"context_tokens"`
		Usage          struct {
			Input      uint64 `json:"input"`
			Output     uint64 `json:"output"`
			CacheRead  uint64 `json:"cache_read"`
			CacheWrite uint64 `json:"cache_write"`
		} `json:"usage"`
		Pricing struct {
			Kind       string  `json:"kind"`
			Cost       float64 `json:"cost"`
			Savings    float64 `json:"savings"`
			Input      float64 `json:"input"`
			Output     float64 `json:"output"`
			CacheRead  float64 `json:"cache_read"`
			CacheWrite float64 `json:"cache_write"`
		} `json:"pricing"`
		Workers []struct {
			ThreadID  string `json:"thread_id"`
			Title     string `json:"title"`
			Status    string `json:"status"`
			MirrorURL string `json:"mirror_url"`
		} `json:"workers"`
		ParentOrchestrator string `json:"parent_orchestrator"`
	}
	if err := s.engine.Catalog(ctx, engine.Catalog{Kind: engine.CatalogThreadStatus, Root: s.root(chat), ThreadID: &threadID}, &body); err != nil {
		return commands.ThreadStatus{}, err
	}
	st := commands.ThreadStatus{
		ConfigError: body.ConfigError, Orchestrator: body.Orchestrator, Refused: body.Refused,
		Model: body.Model, ModelSource: body.ModelSource, Mode: body.Mode, Thinking: body.Thinking, ThinkingSource: body.ThinkingSource,
		ThreadID: body.ThreadID, Root: body.Root, Branch: body.Branch, ContextLimit: body.ContextLimit, ContextTokens: body.ContextTokens,
		Usage: commands.Usage{Input: body.Usage.Input, Output: body.Usage.Output, CacheRead: body.Usage.CacheRead, CacheWrite: body.Usage.CacheWrite},
		Pricing: commands.Pricing{Kind: body.Pricing.Kind, Cost: body.Pricing.Cost, Savings: body.Pricing.Savings,
			Input: body.Pricing.Input, Output: body.Pricing.Output, CacheRead: body.Pricing.CacheRead, CacheWrite: body.Pricing.CacheWrite},
		ParentOrchestrator: body.ParentOrchestrator,
	}
	for _, w := range body.Workers {
		st.Workers = append(st.Workers, commands.StatusWorker{ThreadID: w.ThreadID, Title: w.Title, Status: w.Status, MirrorURL: w.MirrorURL})
	}
	return st, nil
}

func (s serviceHelpers) CustomCommands(ctx context.Context, chat int64, threadID string, native []string) ([]commands.CustomCommand, error) {
	var body struct {
		Commands []struct {
			Name        string `json:"name"`
			Description string `json:"description"`
			Source      string `json:"source"`
			Content     string `json:"content"`
		} `json:"commands"`
	}
	req := engine.Catalog{Kind: engine.CatalogCustomCommands, Root: s.root(chat), ThreadID: optionalID(threadID), Native: native}
	if err := s.engine.Catalog(ctx, req, &body); err != nil {
		return nil, err
	}
	out := make([]commands.CustomCommand, 0, len(body.Commands))
	for _, c := range body.Commands {
		out = append(out, commands.CustomCommand{Name: c.Name, Description: c.Description, Source: c.Source, Content: c.Content})
	}
	return out, nil
}
