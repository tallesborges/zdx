package engine

import (
	"context"
	"encoding/json"
	"fmt"
)

// ReplyTo is the message a user message explicitly replies to.
type ReplyTo struct {
	MessageID int64  `json:"message_id"`
	Text      string `json:"text"`
	FromBot   bool   `json:"from_bot"`
	Quoted    bool   `json:"quoted"`
}

// ComposeAudio is a saved audio attachment and its transcript.
type ComposeAudio struct {
	Path       string  `json:"path"`
	Transcript *string `json:"transcript,omitempty"`
	Label      *string `json:"label,omitempty"`
}

// ComposeDocument is a saved document attachment.
type ComposeDocument struct {
	FileName string `json:"file_name"`
	Path     string `json:"path"`
}

// ComposeImage is a saved image.
type ComposeImage struct {
	Path     string        `json:"path"`
	Label    *string       `json:"label,omitempty"`
	Delivery ImageDelivery `json:"delivery"`
}

// ImageDelivery is how an image reaches the model: Type "inline" (with
// Downscaled) or "saved_only" (with Reason and AskMedia).
type ImageDelivery struct {
	Type       string `json:"type"`
	Downscaled bool   `json:"downscaled,omitempty"`
	Reason     string `json:"reason,omitempty"`
	AskMedia   bool   `json:"ask_media,omitempty"`
}

// MarshalJSON always writes saved_only's reason, which Rust requires.
func (d ImageDelivery) MarshalJSON() ([]byte, error) {
	if d.Type == "saved_only" {
		return json.Marshal(struct {
			Type     string `json:"type"`
			Reason   string `json:"reason"`
			AskMedia bool   `json:"ask_media"`
		}{d.Type, d.Reason, d.AskMedia})
	}
	type plain ImageDelivery
	return json.Marshal(plain(d))
}

// ComposeVideo is a video attachment: Status "saved" with Path, or
// "too_large" with SizeBytes.
type ComposeVideo struct {
	Kind      string  `json:"kind"`
	Status    string  `json:"status"`
	Path      *string `json:"path,omitempty"`
	SizeBytes *uint64 `json:"size_bytes,omitempty"`
}

// ComposeUserText asks Rust for the user text of one Telegram message, the
// wording the bot records and submits (capability compose_user_text). Go
// never builds this text itself.
type ComposeUserText struct {
	ReplyTo   *ReplyTo          `json:"reply_to,omitempty"`
	Text      *string           `json:"text,omitempty"`
	Audios    []ComposeAudio    `json:"audios,omitempty"`
	Images    []ComposeImage    `json:"images,omitempty"`
	Documents []ComposeDocument `json:"documents,omitempty"`
	Videos    []ComposeVideo    `json:"videos,omitempty"`
	Notices   []string          `json:"notices,omitempty"`
}

func (ComposeUserText) op() string { return "compose_user_text" }

// ComposeUserText returns the user text for a message. It needs capability
// compose_user_text and sends nothing without it.
func (c *Client) ComposeUserText(ctx context.Context, req ComposeUserText) (string, error) {
	if err := c.Require(ctx, CapComposeUserText); err != nil {
		return "", err
	}
	reply, err := c.CallOK(ctx, req)
	if err != nil {
		return "", err
	}
	if reply.Kind != ReplyText || reply.Text == nil {
		return "", fmt.Errorf("unexpected reply %q to compose_user_text", reply.Kind)
	}
	return *reply.Text, nil
}
