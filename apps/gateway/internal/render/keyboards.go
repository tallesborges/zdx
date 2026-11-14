package render

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Labels and callback data shared with the Rust bot, so buttons posted by
// either side keep working after a switch.
const (
	OpenArtifactLabel = "↗ Open artifact"
	SendFileLabel     = "⬇ Send file"
	DismissLabel      = "✕ Dismiss"
	// SuggestedRepliesText is the body of the follow-ups message posted when
	// a reply has no text to carry the buttons.
	SuggestedRepliesText = "💡 <i>Suggested replies</i>"

	FollowupPrefix  = "fu:"
	FollowupDismiss = "fu:x"
	// FollowupChosen marks the inert button left after a tap.
	FollowupChosen = "fu:-"
	// SendFilePrefix starts a file card's `⬇ Send file` data: sf:<artifact key>.
	SendFilePrefix = "sf:"
)

// maxButtonChars bounds follow-up button labels (characters).
const maxButtonChars = 64

// maxStartParamLen is the longest Mini App startapp value used: a
// conservative local limit (Telegram documents 64 only for bot start
// parameters), characters limited to A-Z a-z 0-9 _ -.
const maxStartParamLen = 64

type keyboard = telegram.InlineKeyboardMarkup
type button = telegram.InlineKeyboardButton

// FollowupKeyboard is one button per suggestion (data fu:<index>) plus
// Dismiss, or nil when there are none.
func FollowupKeyboard(items []string) *keyboard {
	if len(items) == 0 {
		return nil
	}
	rows := make([][]button, 0, len(items)+1)
	for i, item := range items {
		label := truncateChars(stripMarkdownMarkers(item), maxButtonChars)
		rows = append(rows, []button{telegram.CallbackButton(label, FollowupPrefix+strconv.Itoa(i))})
	}
	rows = append(rows, []button{telegram.CallbackButton(DismissLabel, FollowupDismiss)})
	return &keyboard{InlineKeyboard: rows}
}

// ChosenFollowupKeyboard is the keyboard left after a tap: the kept link
// rows (LinkRows of the tapped message), then the chosen suggestion, inert.
func ChosenFollowupKeyboard(kept [][]button, item string) *keyboard {
	rows := append(append([][]button(nil), kept...), []button{
		telegram.CallbackButton(truncateChars("▶️ "+stripMarkdownMarkers(item), maxButtonChars), FollowupChosen),
	})
	return &keyboard{InlineKeyboard: rows}
}

// FollowupTap is a parsed follow-up callback.
type FollowupTap struct {
	Dismiss bool
	// Index is the tapped suggestion; -1 for Dismiss, the inert button, or
	// data that is not a follow-up index.
	Index int
}

// ParseFollowupData parses callback data that starts with FollowupPrefix.
// ok is false for other data.
func ParseFollowupData(data string) (FollowupTap, bool) {
	rest, ok := strings.CutPrefix(data, FollowupPrefix)
	if !ok {
		return FollowupTap{}, false
	}
	if rest == "x" {
		return FollowupTap{Dismiss: true, Index: -1}, true
	}
	i, err := strconv.ParseUint(rest, 10, 31)
	if err != nil {
		return FollowupTap{Index: -1}, true
	}
	return FollowupTap{Index: int(i)}, true
}

// LinkRows returns the rows of a message's reply_markup in which every
// button only opens a URL (a file's `Open artifact`). Follow-up edits keep
// them, so a captioned file keeps its link after a tap, even after a
// restart.
func LinkRows(replyMarkup json.RawMessage) [][]button {
	var markup struct {
		InlineKeyboard [][]struct {
			Text *string `json:"text"`
			URL  *string `json:"url"`
		} `json:"inline_keyboard"`
	}
	if len(replyMarkup) == 0 || json.Unmarshal(replyMarkup, &markup) != nil {
		return nil
	}
	var rows [][]button
	for _, row := range markup.InlineKeyboard {
		links := make([]button, 0, len(row))
		for _, b := range row {
			if b.Text == nil || b.URL == nil {
				links = nil
				break
			}
			links = append(links, telegram.URLButton(*b.Text, *b.URL))
		}
		if len(links) > 0 {
			rows = append(rows, links)
		}
	}
	return rows
}

// OpenArtifactKeyboard is the file action of one file message.
func OpenArtifactKeyboard(url string) *keyboard {
	return &keyboard{InlineKeyboard: [][]button{{telegram.URLButton(OpenArtifactLabel, url)}}}
}

// CaptionedKeyboard is the keyboard of a captioned file: its file action
// first (when url is set), then the follow-ups; nil when both are empty.
func CaptionedKeyboard(openURL string, followups *keyboard) *keyboard {
	var rows [][]button
	if openURL != "" {
		rows = append(rows, OpenArtifactKeyboard(openURL).InlineKeyboard...)
	}
	if followups != nil {
		rows = append(rows, followups.InlineKeyboard...)
	}
	if len(rows) == 0 {
		return nil
	}
	return &keyboard{InlineKeyboard: rows}
}

// CardKeyboard is a file card's `Open artifact` and `Send file` buttons.
// artifactKey is the engine's artifact_key of the file.
func CardKeyboard(openURL, artifactKey string) *keyboard {
	return &keyboard{InlineKeyboard: [][]button{{
		telegram.URLButton(OpenArtifactLabel, openURL),
		telegram.CallbackButton(SendFileLabel, SendFilePrefix+artifactKey),
	}}}
}

// CardText is the body of a file card: name, size, and that both buttons
// use the file as it is when tapped.
func CardText(fileName string, size int64) string {
	return fmt.Sprintf("📄 <b>%s</b>\n%s · HTML · not uploaded\n<i>Open artifact and Send file use the file as it is now on the Mac.</i>",
		EscapeHTML(fileName), formatBytes(size))
}

func formatBytes(n int64) string {
	if n < 1024 {
		return fmt.Sprintf("%d B", n)
	}
	value, unit := float64(n)/1024, "KB"
	for _, next := range []string{"MB", "GB"} {
		if value < 1024 {
			break
		}
		value /= 1024
		unit = next
	}
	if value >= 100 {
		return fmt.Sprintf("%.0f %s", value, unit)
	}
	return fmt.Sprintf("%.1f %s", value, unit)
}

// ArtifactLink is the Mini App deep link that opens one artifact:
// <base>?startapp=a_<key>_<thread>. thread is the session id of the topic
// that receives the reply (before any alias hop), so feedback from the
// preview returns there. ok is false when the parameter would exceed 64
// characters or leave Telegram's start-parameter alphabet.
func ArtifactLink(base, threadID, artifactKey string) (string, bool) {
	param := "a_" + artifactKey + "_" + threadID
	if len(param) > maxStartParamLen || !allRunes(param, isStartParamRune) {
		return "", false
	}
	return base + "?startapp=" + param, true
}

func isStartParamRune(r rune) bool { return isASCIIAlnum(r) || r == '_' || r == '-' }

// AppendThreadLink adds the `↗ Open thread` Mini App link to reply HTML;
// text is unchanged when base is empty (no Mini App for the chat). Send the
// result without link previews.
func AppendThreadLink(base, threadID, text string) string {
	if base == "" {
		return text
	}
	return text + "\n\n<a href=\"" + base + "?startapp=" + threadID + "\">↗ Open thread</a>"
}

// WorkerLink is one worker an orchestrator turn created or messaged.
type WorkerLink struct {
	// Title is the worker thread's title; empty uses the first 8
	// characters of ID.
	Title string
	ID    string
	URL   string // see WorkerURL
}

// WorkerURL is where a worker link points: its legacy mirror topic when it
// has one, else the worker thread in the Mini App. ok is false when neither
// exists.
func WorkerURL(mirrorURL, miniAppBase, workerThreadID string) (string, bool) {
	switch {
	case mirrorURL != "":
		return mirrorURL, true
	case miniAppBase != "":
		return miniAppBase + "?startapp=" + workerThreadID, true
	}
	return "", false
}

// AppendWorkerLinks adds one `🛠 <title>` link per worker after a blank
// line; text is unchanged without links.
func AppendWorkerLinks(links []WorkerLink, text string) string {
	if len(links) == 0 {
		return text
	}
	var b strings.Builder
	b.WriteString(text)
	b.WriteString("\n")
	for _, l := range links {
		title := l.Title
		if title == "" {
			title = firstChars(l.ID, 8)
		}
		fmt.Fprintf(&b, "\n🛠 <a href=\"%s\">%s</a>", l.URL, EscapeHTML(title))
	}
	return b.String()
}

// stripMarkdownMarkers removes Markdown markers that would show literally
// on a plain-text button. Underscores stay: identifiers use them.
func stripMarkdownMarkers(s string) string {
	s = strings.ReplaceAll(s, "__", "")
	s = strings.NewReplacer("`", "", "*", "").Replace(s)
	return strings.TrimSpace(s)
}

// truncateChars keeps at most limit characters, ending with "…" when cut.
func truncateChars(s string, limit int) string {
	if utf8.RuneCountInString(s) <= limit {
		return s
	}
	return firstChars(s, max(limit-1, 0)) + "…"
}

func firstChars(s string, n int) string {
	i := 0
	for pos := range s {
		if i == n {
			return s[:pos]
		}
		i++
	}
	return s
}
