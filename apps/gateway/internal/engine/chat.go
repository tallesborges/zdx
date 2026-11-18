package engine

import (
	"context"
	"fmt"
)

// Thread kinds of resolve_thread.
const (
	KindOrdinary     = "ordinary"
	KindProfile      = "profile"
	KindWorkerMirror = "worker_mirror"
)

// ResolveThread asks where a chat conversation's messages go (capability
// resolve_thread).
type ResolveThread struct {
	ChatID  int64  `json:"chat_id"`
	TopicID *int64 `json:"topic_id,omitempty"`
}

func (ResolveThread) op() string { return "resolve_thread" }

// ThreadRoute is zdx_engine::orchestration::thread_ops::ThreadRoute.
type ThreadRoute struct {
	// ThreadID is the conversation's own id.
	ThreadID string
	// EffectiveThreadID is the thread turns run on, after one alias hop:
	// use it for submit, deliveries and updates (the worker for a mirror).
	EffectiveThreadID string
	Kind              string
	Profile           *string
	Exists            bool
	Root              *string
	PendingTopicTitle bool
}

// ResolveThread returns a conversation's route. It needs capability
// resolve_thread.
func (c *Client) ResolveThread(ctx context.Context, chat int64, topic *int64) (ThreadRoute, error) {
	if err := c.Require(ctx, CapResolveThread); err != nil {
		return ThreadRoute{}, err
	}
	reply, err := c.CallOK(ctx, ResolveThread{ChatID: chat, TopicID: topic})
	if err != nil {
		return ThreadRoute{}, err
	}
	if reply.Kind != ReplyThreadRoute || reply.EffectiveThreadID == "" {
		return ThreadRoute{}, fmt.Errorf("unexpected reply %q to resolve_thread", reply.Kind)
	}
	return ThreadRoute{
		ThreadID: reply.ThreadID, EffectiveThreadID: reply.EffectiveThreadID, Kind: reply.RouteKind,
		Profile: reply.Profile, Exists: reply.Exists, Root: reply.Root, PendingTopicTitle: reply.PendingTopicTitle,
	}, nil
}

// SendAs values of reply media.
const (
	SendAsPhoto    = "photo"
	SendAsVoice    = "voice"
	SendAsAudio    = "audio"
	SendAsVideo    = "video"
	SendAsDocument = "document"
)

// ReplyMedia is one `<media>` file of a reply, as Rust classifies it.
type ReplyMedia struct {
	Path        string `json:"path"`
	ArtifactKey string `json:"artifact_key"`
	// Kind is the artifact kind: image, audio, video, html, markdown, text, other.
	Kind              string `json:"kind"`
	SendAs            string `json:"send_as"`
	LexicalIsPhysical bool   `json:"lexical_is_physical"`
	// Sendable: an absolute path to an existing regular file. Only these are
	// sent; the others fail without a send.
	Sendable bool `json:"sendable"`
}

// ReplyPartsRequest splits a final answer (capability reply_parts).
type ReplyPartsRequest struct {
	FinalText string `json:"final_text"`
}

func (ReplyPartsRequest) op() string { return "reply_parts" }

// ReplyParts is a final answer split for a chat: the reply Markdown without
// follow-ups and media blocks, the follow-ups, and each file once in order.
type ReplyParts struct {
	Text      string
	Followups []string
	Media     []ReplyMedia
}

// ReplyParts splits a final answer. It needs capability reply_parts.
func (c *Client) ReplyParts(ctx context.Context, finalText string) (ReplyParts, error) {
	if err := c.Require(ctx, CapReplyParts); err != nil {
		return ReplyParts{}, err
	}
	reply, err := c.CallOK(ctx, ReplyPartsRequest{FinalText: finalText})
	if err != nil {
		return ReplyParts{}, err
	}
	if reply.Kind != ReplyReplyParts || reply.Text == nil {
		return ReplyParts{}, fmt.Errorf("unexpected reply %q to reply_parts", reply.Kind)
	}
	return ReplyParts{Text: *reply.Text, Followups: reply.Followups, Media: reply.Media}, nil
}
