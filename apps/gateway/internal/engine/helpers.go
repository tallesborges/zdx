package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"time"
)

// Capabilities of the helper and catalog operations (Rust contracts v9 and
// v10).
const (
	CapBtwSeed      = "btw_seed"
	CapTopicStart   = "topic_start"
	CapCatalog      = "catalog"
	CapSetChatModel = "set_chat_model"
)

// Reply kinds of the helper and catalog operations.
const (
	ReplyGenerated  = "generated"
	ReplyTopicStart = "topic_start"
	ReplyCatalog    = "catalog"
	ReplyChatModel  = "chat_model"
)

// HelperTimeout bounds one generation. Rust's helpers have their own
// limits (a title tries two models for up to a minute each; handoff and
// prompt builder up to two minutes); this only stops waiting for an answer
// that will not come.
const HelperTimeout = 5 * time.Minute

// Generation kinds.
const (
	GenerateTitle         = "title"
	GenerateTLDR          = "tldr"
	GenerateHandoff       = "handoff"
	GeneratePromptBuilder = "prompt_builder"
)

// Generate asks Rust for helper text (capability generate). Root is the
// chat's root; ThreadID is required for tldr and handoff.
type Generate struct {
	Kind     string  `json:"kind"`
	Root     string  `json:"root"`
	Text     string  `json:"text,omitempty"`
	ThreadID *string `json:"thread_id,omitempty"`
	Goal     string  `json:"goal,omitempty"`
}

func (Generate) op() string { return "generate" }

// Generate runs one generation and returns its text. A helper failure
// (timeout, model error) is a *ServiceError without a code. The service
// finishes a generation whose client went away and drops the answer.
func (c *Client) Generate(ctx context.Context, g Generate) (string, error) {
	if err := c.Require(ctx, CapGenerate); err != nil {
		return "", err
	}
	reply, err := c.CallWithin(ctx, g, HelperTimeout)
	if err != nil {
		return "", err
	}
	if err := reply.Err(); err != nil {
		return "", err
	}
	if reply.Kind != ReplyGenerated || reply.Text == nil {
		return "", fmt.Errorf("unexpected reply %q to generate", reply.Kind)
	}
	return *reply.Text, nil
}

// BtwSeed is the /btw seed request (capability btw_seed).
type BtwSeed struct {
	ThreadID string `json:"thread_id"`
	Question string `json:"question"`
}

func (BtwSeed) op() string { return "btw_seed" }

// BtwSeed returns the /btw seed: the source header with lineage, then the
// question verbatim. No model call.
func (c *Client) BtwSeed(ctx context.Context, threadID, question string) (string, error) {
	if err := c.Require(ctx, CapBtwSeed); err != nil {
		return "", err
	}
	reply, err := c.CallOK(ctx, BtwSeed{ThreadID: threadID, Question: question})
	if err != nil {
		return "", err
	}
	if reply.Kind != ReplyText || reply.Text == nil {
		return "", fmt.Errorf("unexpected reply %q to btw_seed", reply.Kind)
	}
	return *reply.Text, nil
}

// TopicStart asks for the starting model of a topic seeded from
// SourceThreadID (capability topic_start). Profile is "orchestrator" or
// empty; with Pick, the pick is resolved against that start.
type TopicStart struct {
	Root           string `json:"root"`
	SourceThreadID string `json:"source_thread_id"`
	Profile        string `json:"profile,omitempty"`
	Pick           string `json:"pick,omitempty"`
}

func (TopicStart) op() string { return "topic_start" }

// TopicStarted is the answer to topic_start. Selection nil: create the
// thread without a model.
type TopicStarted struct {
	Selection *string
	Label     string
}

// TopicStart resolves a seeded topic's start. invalid (a *ServiceError)
// means nothing may be created.
func (c *Client) TopicStart(ctx context.Context, t TopicStart) (TopicStarted, error) {
	if err := c.Require(ctx, CapTopicStart); err != nil {
		return TopicStarted{}, err
	}
	reply, err := c.CallOK(ctx, t)
	if err != nil {
		return TopicStarted{}, err
	}
	if reply.Kind != ReplyTopicStart || reply.Label == nil {
		return TopicStarted{}, fmt.Errorf("unexpected reply %q to topic_start", reply.Kind)
	}
	return TopicStarted{Selection: reply.Selection, Label: *reply.Label}, nil
}

// Catalog is one catalog query (capability catalog). Fields are sent as
// given; Kind names the query.
type Catalog struct {
	Kind     string   `json:"kind"`
	Root     string   `json:"root,omitempty"`
	Scope    string   `json:"scope,omitempty"`
	ThreadID *string  `json:"thread_id,omitempty"`
	Profile  string   `json:"profile,omitempty"`
	Role     string   `json:"role,omitempty"`
	Native   []string `json:"native,omitempty"`
}

func (Catalog) op() string { return "catalog" }

// Catalog kinds.
const (
	CatalogPicker         = "picker"
	CatalogLauncher       = "launcher"
	CatalogResumable      = "resumable"
	CatalogResumeSource   = "resume_source"
	CatalogThreadStatus   = "thread_status"
	CatalogCustomCommands = "custom_commands"
)

// Catalog runs a catalog query and decodes its body into out.
func (c *Client) Catalog(ctx context.Context, q Catalog, out any) error {
	if err := c.Require(ctx, CapCatalog); err != nil {
		return err
	}
	reply, err := c.CallOK(ctx, q)
	if err != nil {
		return err
	}
	if reply.Kind != ReplyCatalog || len(reply.Body) == 0 {
		return fmt.Errorf("unexpected reply %q to catalog %s", reply.Kind, q.Kind)
	}
	if err := json.Unmarshal(reply.Body, out); err != nil {
		return fmt.Errorf("decode catalog %s: %w", q.Kind, err)
	}
	return nil
}

// SetChatModel writes the chat's default model (capability set_chat_model).
type SetChatModel struct {
	Root  string `json:"root"`
	Model string `json:"model"`
}

func (SetChatModel) op() string { return "set_chat_model" }

// SetChatModel validates model against the chat's config, stores it in the
// workspace config of root, and returns the stored selection.
func (c *Client) SetChatModel(ctx context.Context, root, model string) (string, error) {
	if err := c.Require(ctx, CapSetChatModel); err != nil {
		return "", err
	}
	reply, err := c.CallOK(ctx, SetChatModel{Root: root, Model: model})
	if err != nil {
		return "", err
	}
	if reply.Kind != ReplyChatModel || reply.Selection == nil {
		return "", fmt.Errorf("unexpected reply %q to set_chat_model", reply.Kind)
	}
	return *reply.Selection, nil
}
