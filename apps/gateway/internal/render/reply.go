package render

import (
	"path/filepath"
	"strings"
)

// Reply is one final reply. The gateway fills it from the engine's
// reply_parts answer; render derives none of these fields.
type Reply struct {
	// HTML is reply_parts `text` converted with ToTelegramHTML.
	HTML      string
	Followups []string
	// Files are every reply_parts `media` entry, in order, sendable or not
	// (never filter by sendable; set Missing):
	// a file that cannot be sent is still a failed delivery item, as in the
	// Rust bot, so the record and its user summary name it.
	Files []File
}

// File is one reply_parts media entry.
type File struct {
	Path string
	// Missing is !sendable (not an absolute path to a regular file). The
	// item fails with FileNotFound without a send and is never captioned.
	Missing bool
	// Size in bytes, shown on a file card (reply_parts has no size; the
	// Rust card shows 0 B when the file cannot be read).
	Size int64
	// ArtifactKey is `artifact_key`.
	ArtifactKey string
	// Linkable is `lexical_is_physical`: otherwise the file gets no Open
	// artifact link rather than one to another file.
	Linkable bool
	// HTML is `kind == "html"`: with a link the file goes out as a file
	// card instead of an upload.
	HTML bool
	// Method is `send_as` (MethodFromSendAs). A sendable file without a
	// valid method fails with NoSendMethod, without a send; a Missing file
	// fails with FileNotFound whatever its method.
	Method Method
}

// Options are the per-chat presentation inputs of a reply.
type Options struct {
	// MiniAppBase is the chat's Mini App URL; empty disables every link.
	MiniAppBase string
	// ThreadID is the thread the reply belongs to (Open thread link).
	ThreadID string
	// TopicThreadID is the session id of the receiving chat or topic, before
	// any alias hop (artifact links).
	TopicThreadID string
	// Workers are the workers an orchestrator turn created or messaged, with
	// their URLs resolved (WorkerURL).
	Workers []WorkerLink
}

// ItemKind is the kind of one delivery item.
type ItemKind int

const (
	ItemText ItemKind = iota
	ItemFile
)

// Item is one delivery item, in record order: text parts first, then one
// per file. It maps to zdx_engine::core::delivery::DeliveryItem
// (text(), text_part(index, total), file(path)).
type Item struct {
	Kind ItemKind
	// Part and Total number the message when reply text was split; both 0
	// when it fits one message.
	Part, Total int
	Path        string
	// FailedBeforeSend holds the error of an item that fails without a
	// send: FileNotFound or NoSendMethod.
	FailedBeforeSend string
}

// StepKind is what one send is.
type StepKind int

const (
	// StepText sends Text with sendMessage, without link preview.
	StepText StepKind = iota
	// StepFile uploads File with Method, no caption.
	StepFile
	// StepCard sends Text (a file card) with sendMessage; nothing is
	// uploaded. Set link_only on the file item when the step is attempted.
	StepCard
	// StepCaptioned uploads File with Method and Text as its caption. It
	// settles the text item and the file item together.
	StepCaptioned
)

// Step is one send of a plan.
type Step struct {
	Kind StepKind
	// Text is the message HTML (StepText, StepCard) or the caption
	// (StepCaptioned).
	Text     string
	File     *File
	Method   Method
	Keyboard *keyboard
	// Items are the indexes into Plan.Items this step settles.
	Items []int
	// Followups: this message carries the follow-up keyboard; register
	// Plan.Followups under its message id when it is sent.
	Followups bool
	// Fallback (StepCaptioned only) is sent when Telegram refuses the file
	// (telegram.ClassRejected or ClassNotSent): the file item fails and the
	// text goes out alone. A flood refusal fails both items; an ambiguous
	// failure makes both unknown.
	Fallback *Step
}

// Plan is how one reply is delivered.
type Plan struct {
	Items []Item
	Steps []Step
	// Followups are the suggestions, for registering taps.
	Followups []string
	// SeparateFollowups: the reply has no text, so the follow-ups go out as
	// their own SuggestedRepliesText message with FollowupKeyboard, after
	// the delivery is recorded (Rust posts it on the paced client).
	SeparateFollowups bool
}

// Errors of file items that fail before any send.
const (
	// FileNotFound: the file is Missing (reply_parts sendable=false). Same
	// wording as the Rust bot's delivery record.
	FileNotFound = "file not found or not a regular file"
	// NoSendMethod: the file has no valid Method (a gateway mapping error).
	NoSendMethod = "no send method for this file"
)

// PlanReply plans a reply the way handlers/message/response.rs delivers it:
//
//   - The text gets the worker links and the Open thread link, then is split
//     into messages of at most MaxMessageLen (SplitHTML).
//   - Text that fits one caption (VisibleUTF16Len <= MaxCaptionLen) plus a
//     single photo, voice note or audio file go out as one captioned message
//     carrying the file's Open artifact button and the follow-ups.
//   - Otherwise the text parts go first, the follow-ups on the last part,
//     then every file as its own message with its own Open artifact button.
//     An HTML file with a link is a file card, never uploaded. Documents are
//     never merged or captioned with the reply.
//
// A plan with no items means nothing to deliver; follow-ups, if any, then
// go out separately.
func PlanReply(r Reply, o Options) Plan {
	p := Plan{Followups: r.Followups}
	hasText := strings.TrimSpace(r.HTML) != ""
	var parts []string
	if hasText {
		text := AppendWorkerLinks(o.Workers, r.HTML)
		text = AppendThreadLink(o.MiniAppBase, o.ThreadID, text)
		parts = SplitHTML(text, MaxMessageLen)
	}
	p.SeparateFollowups = !hasText && len(r.Followups) > 0

	for i := range parts {
		item := Item{Kind: ItemText}
		if len(parts) > 1 {
			item.Part, item.Total = i+1, len(parts)
		}
		p.Items = append(p.Items, item)
	}
	fileOffset := len(p.Items)
	for _, f := range r.Files {
		item := Item{Kind: ItemFile, Path: f.Path}
		switch {
		case f.Missing:
			item.FailedBeforeSend = FileNotFound
		case !f.Method.valid():
			item.FailedBeforeSend = NoSendMethod
		}
		p.Items = append(p.Items, item)
	}

	var followups *keyboard
	if hasText {
		followups = FollowupKeyboard(r.Followups)
	}
	openURL := func(f *File) string {
		if o.MiniAppBase == "" || !f.Linkable {
			return ""
		}
		url, _ := ArtifactLink(o.MiniAppBase, o.TopicThreadID, f.ArtifactKey)
		return url
	}

	if len(parts) == 1 && len(r.Files) == 1 {
		f := &r.Files[0]
		if p.Items[fileOffset].FailedBeforeSend == "" && f.Method.takesReplyCaption() && VisibleUTF16Len(parts[0]) <= MaxCaptionLen {
			p.Steps = []Step{{
				Kind: StepCaptioned, Text: parts[0], File: f, Method: f.Method,
				Keyboard: CaptionedKeyboard(openURL(f), followups), Items: []int{0, 1},
				Followups: followups != nil,
				Fallback: &Step{
					Kind: StepText, Text: parts[0], Keyboard: followups, Items: []int{0},
					Followups: followups != nil,
				},
			}}
			return p
		}
	}

	for i, part := range parts {
		step := Step{Kind: StepText, Text: part, Items: []int{i}}
		if i == len(parts)-1 {
			step.Keyboard = followups
			step.Followups = followups != nil
		}
		p.Steps = append(p.Steps, step)
	}
	for i := range r.Files {
		f := &r.Files[i]
		if p.Items[fileOffset+i].FailedBeforeSend != "" {
			continue
		}
		url := openURL(f)
		step := Step{File: f, Items: []int{fileOffset + i}}
		switch {
		case url != "" && f.HTML:
			step.Kind = StepCard
			step.Text = CardText(filepath.Base(f.Path), f.Size)
			step.Keyboard = CardKeyboard(url, f.ArtifactKey)
		default:
			step.Kind = StepFile
			step.Method = f.Method
			if url != "" {
				step.Keyboard = OpenArtifactKeyboard(url)
			}
		}
		p.Steps = append(p.Steps, step)
	}
	return p
}
