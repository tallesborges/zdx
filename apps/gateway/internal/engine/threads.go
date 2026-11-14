package engine

import (
	"context"
	"encoding/json"
	"fmt"
)

// Reply kind of update_thread.
const ReplyThreadUpdated = "thread_updated"

// UpdateThread changes thread metadata (capability update_thread). Only the
// fields this gateway sets are modelled; nil and empty fields are left out.
// Model is a spec (or `mode:<name>`) to set, or ClearModel to remove the
// override. HandoffFrom and PersistentProfile are taken only by a thread
// Rust has not written yet.
type UpdateThread struct {
	ThreadID   string
	ConfigRoot *string
	// Model: nil leaves it unchanged; a pointer to "" is not allowed, use
	// ClearModel.
	Model      *string
	ClearModel bool
	// Root is the folder the thread runs in.
	Root *string
	// AliasTo makes the thread resume another one.
	AliasTo           *string
	PendingTopicTitle *bool
	// Title stores the thread's title.
	Title             *string
	HandoffFrom       *string
	PersistentProfile *string
}

func (UpdateThread) op() string { return "update_thread" }

// MarshalJSON writes Rust's three-state model field: absent (unchanged), a
// spec, or null (clear).
func (u UpdateThread) MarshalJSON() ([]byte, error) {
	fields := map[string]any{"thread_id": u.ThreadID}
	if u.ConfigRoot != nil {
		fields["config_root"] = *u.ConfigRoot
	}
	switch {
	case u.ClearModel:
		fields["model"] = nil
	case u.Model != nil:
		fields["model"] = *u.Model
	}
	for key, v := range map[string]*string{"root": u.Root, "alias_to": u.AliasTo, "handoff_from": u.HandoffFrom, "persistent_profile": u.PersistentProfile, "title": u.Title} {
		if v != nil {
			fields[key] = *v
		}
	}
	if u.PendingTopicTitle != nil {
		fields["pending_topic_title"] = *u.PendingTopicTitle
	}
	return json.Marshal(fields)
}

// ThreadUpdated is the answer to update_thread.
type ThreadUpdated struct {
	ThreadID   string
	ModelLabel *string
	Title      *string
}

// UpdateThread applies a metadata change. Rust validates the model against
// the config of config_root (or the stored root). It needs capability
// update_thread.
func (c *Client) UpdateThread(ctx context.Context, u UpdateThread) (ThreadUpdated, error) {
	if u.ClearModel && u.Model != nil {
		return ThreadUpdated{}, fmt.Errorf("update_thread: set a model or clear it, not both")
	}
	if err := c.Require(ctx, CapUpdateThread); err != nil {
		return ThreadUpdated{}, err
	}
	reply, err := c.CallOK(ctx, u)
	if err != nil {
		return ThreadUpdated{}, err
	}
	if reply.Kind != ReplyThreadUpdated {
		return ThreadUpdated{}, fmt.Errorf("unexpected reply %q to update_thread", reply.Kind)
	}
	return ThreadUpdated{ThreadID: reply.ThreadID, ModelLabel: reply.ModelLabel, Title: reply.Title}, nil
}

// ResetThread deletes a thread's history (capability reset_thread). The
// service refuses it while the thread has work queued, running or held.
type ResetThread struct {
	ThreadID string `json:"thread_id"`
}

func (ResetThread) op() string { return "reset_thread" }

// ResetThread clears a thread. It needs capability reset_thread.
func (c *Client) ResetThread(ctx context.Context, threadID string) error {
	if err := c.Require(ctx, CapResetThread); err != nil {
		return err
	}
	reply, err := c.CallOK(ctx, ResetThread{ThreadID: threadID})
	if err != nil {
		return err
	}
	if reply.Kind != ReplyOK {
		return fmt.Errorf("unexpected reply %q to reset_thread", reply.Kind)
	}
	return nil
}
