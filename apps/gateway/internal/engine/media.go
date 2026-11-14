package engine

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
)

// Reply kinds of the media operations.
const (
	ReplyPreparedImage = "prepared_image"
	ReplyTranscript    = "transcript"
	ReplyPreparedPhoto = "prepared_photo"
)

// PreparePhoto asks Rust to make a reply photo ready for sendPhoto
// (capability prepare_photo; zdx_engine::telegram::photo).
type PreparePhoto struct {
	Path string `json:"path"`
}

func (PreparePhoto) op() string { return "prepare_photo" }

// PreparedPhoto is the answer to prepare_photo. Without Changed the file
// is uploaded as it is, under FileName and MimeType; with it, Data.
type PreparedPhoto struct {
	Changed  bool
	FileName string
	MimeType string
	Data     []byte
}

// PreparePhoto prepares a reply photo. A missing file is a *ServiceError
// with code not_found; a photo that cannot be prepared (undecodable where
// it must change, or still too large) is a *ServiceError without a code.
func (c *Client) PreparePhoto(ctx context.Context, path string) (PreparedPhoto, error) {
	if err := c.Require(ctx, CapPreparePhoto); err != nil {
		return PreparedPhoto{}, err
	}
	reply, err := c.CallOK(ctx, PreparePhoto{Path: path})
	if err != nil {
		return PreparedPhoto{}, err
	}
	if reply.Kind != ReplyPreparedPhoto || reply.FileName == "" || reply.MimeType == "" || reply.Changed != (reply.Data != nil) {
		return PreparedPhoto{}, fmt.Errorf("unexpected reply %q to prepare_photo", reply.Kind)
	}
	out := PreparedPhoto{Changed: reply.Changed, FileName: reply.FileName, MimeType: reply.MimeType}
	if reply.Changed {
		if out.Data, err = base64.StdEncoding.DecodeString(*reply.Data); err != nil {
			return PreparedPhoto{}, fmt.Errorf("decode prepare_photo data: %w", err)
		}
	}
	return out, nil
}

// PrepareImage asks Rust how a saved incoming image reaches the model
// (capability prepare_image): an inline copy fitted to the model limits, or
// saved only with the reason.
type PrepareImage struct {
	Path string `json:"path"`
}

func (PrepareImage) op() string { return "prepare_image" }

// PreparedImage is the answer to prepare_image. Image is set exactly when
// Delivery.Type is "inline".
type PreparedImage struct {
	Delivery ImageDelivery
	Image    *InputImage
}

// PrepareImage prepares a saved image. A missing file is a *ServiceError
// with code not_found.
func (c *Client) PrepareImage(ctx context.Context, path string) (PreparedImage, error) {
	if err := c.Require(ctx, CapPrepareImage); err != nil {
		return PreparedImage{}, err
	}
	reply, err := c.CallOK(ctx, PrepareImage{Path: path})
	if err != nil {
		return PreparedImage{}, err
	}
	if reply.Kind != ReplyPreparedImage || len(reply.Delivery) == 0 {
		return PreparedImage{}, fmt.Errorf("unexpected reply %q to prepare_image", reply.Kind)
	}
	var out PreparedImage
	if err := json.Unmarshal(reply.Delivery, &out.Delivery); err != nil {
		return PreparedImage{}, fmt.Errorf("decode prepare_image delivery: %w", err)
	}
	out.Image = reply.Image
	inline := out.Delivery.Type == "inline"
	if (inline != (out.Image != nil)) || (!inline && out.Delivery.Type != "saved_only") {
		return PreparedImage{}, fmt.Errorf("prepare_image: inconsistent delivery %q", out.Delivery.Type)
	}
	return out, nil
}

// TranscribeAudio transcribes a saved incoming audio file with the
// transcription config of Root, the chat's root (capability
// transcribe_audio).
type TranscribeAudio struct {
	Path     string  `json:"path"`
	FileName string  `json:"file_name"`
	MimeType *string `json:"mime_type,omitempty"`
	Root     string  `json:"root"`
}

func (TranscribeAudio) op() string { return "transcribe_audio" }

// TranscribeAudio returns the transcript, nil when no transcription
// provider is configured. A provider failure is an error; the Rust bot
// then continues the turn without a transcript. Transcription can take
// minutes, so it gets TranscribeTimeout.
func (c *Client) TranscribeAudio(ctx context.Context, req TranscribeAudio) (*string, error) {
	if err := c.Require(ctx, CapTranscribeAudio); err != nil {
		return nil, err
	}
	reply, err := c.CallWithin(ctx, req, TranscribeTimeout)
	if err != nil {
		return nil, err
	}
	if err := reply.Err(); err != nil {
		return nil, err
	}
	if reply.Kind != ReplyTranscript {
		return nil, fmt.Errorf("unexpected reply %q to transcribe_audio", reply.Kind)
	}
	return reply.Transcript, nil
}
