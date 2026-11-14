package render

import (
	"context"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Sender sends one step. Implementations own the Telegram details: reply
// target and topic, the reliable client mode, link previews off for text,
// the HTML repair fallback, and the one retry without reply_to after an
// invalid reply target. They return the sent message id.
type Sender interface {
	SendText(ctx context.Context, html string, kb *telegram.InlineKeyboardMarkup) (int64, error)
	// SendFile uploads f with method; caption is nil for a plain file.
	SendFile(ctx context.Context, f File, method Method, caption *string, kb *telegram.InlineKeyboardMarkup) (int64, error)
}

// Status is a delivery item outcome, as DeliveryStatus names it.
type Status string

const (
	StatusPending Status = "pending"
	StatusSent    Status = "sent"
	StatusFailed  Status = "failed"
	StatusUnknown Status = "unknown"
)

// Outcome is what happened to one Plan item.
type Outcome struct {
	Status    Status
	MessageID *int64
	Error     string
	// LinkOnly: a file card was posted instead of uploading the file.
	LinkOnly bool
}

// Result is the outcome of a delivered plan, item by item, for the
// delivery record.
type Result struct {
	Items []Outcome
	// FollowupsMessageID is the message that carries the follow-ups, when
	// it was sent; register Plan.Followups under it.
	FollowupsMessageID *int64
}

// Deliver runs a plan in order, as deliver_reply does. A refused or
// unknown send does not stop later steps, except a flood refusal the
// client could not absorb: the remaining items are failed without an
// attempt. An ambiguous failure is unknown and is never resent.
func Deliver(ctx context.Context, s Sender, p Plan) Result {
	res := Result{Items: make([]Outcome, len(p.Items))}
	for i, item := range p.Items {
		res.Items[i].Status = StatusPending
		if item.FailedBeforeSend != "" {
			res.Items[i] = Outcome{Status: StatusFailed, Error: item.FailedBeforeSend}
		}
	}
	flood := ""
	for _, step := range p.Steps {
		if flood != "" {
			for _, i := range step.Items {
				res.Items[i] = Outcome{Status: StatusFailed, Error: "not attempted after a flood refusal: " + flood}
			}
			continue
		}
		if step.Kind == StepCaptioned {
			deliverCaptioned(ctx, s, step, &res)
			continue
		}
		if step.Kind == StepCard {
			for _, i := range step.Items {
				res.Items[i].LinkOnly = true
			}
		}
		id, err := send(ctx, s, step)
		if class, ok := settle(&res, step, id, err); !ok && class == telegram.ClassFloodWait {
			flood = err.Error()
		}
	}
	return res
}

func deliverCaptioned(ctx context.Context, s Sender, step Step, res *Result) {
	id, err := send(ctx, s, step)
	if err == nil {
		settle(res, step, id, nil)
		return
	}
	class, _ := telegram.Classify(err)
	switch class {
	case telegram.ClassUnknown:
		mark(res, step.Items, Outcome{Status: StatusUnknown, Error: err.Error()})
	case telegram.ClassFloodWait:
		mark(res, step.Items, Outcome{Status: StatusFailed, Error: err.Error()})
	default:
		// The file itself was refused; the text can still go out.
		res.Items[step.Items[1]] = Outcome{Status: StatusFailed, Error: err.Error()}
		fb := *step.Fallback
		id, err := send(ctx, s, fb)
		settle(res, fb, id, err)
	}
}

func send(ctx context.Context, s Sender, step Step) (int64, error) {
	switch step.Kind {
	case StepFile:
		return s.SendFile(ctx, *step.File, step.Method, nil, step.Keyboard)
	case StepCaptioned:
		caption := step.Text
		return s.SendFile(ctx, *step.File, step.Method, &caption, step.Keyboard)
	default:
		return s.SendText(ctx, step.Text, step.Keyboard)
	}
}

// settle records a send on the step's items. It returns the failure class
// and false on error.
func settle(res *Result, step Step, id int64, err error) (telegram.FailureClass, bool) {
	if err == nil {
		for _, i := range step.Items {
			res.Items[i].Status = StatusSent
			res.Items[i].MessageID = &id
			res.Items[i].Error = ""
		}
		if step.Followups {
			res.FollowupsMessageID = &id
		}
		return 0, true
	}
	class, _ := telegram.Classify(err)
	status := StatusFailed
	if !class.DefinitelyUndelivered() {
		status = StatusUnknown
	}
	for _, i := range step.Items {
		res.Items[i].Status = status
		res.Items[i].Error = err.Error()
	}
	return class, false
}

func mark(res *Result, items []int, o Outcome) {
	for _, i := range items {
		res.Items[i] = o
	}
}
