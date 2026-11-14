package render

import "fmt"

// Method is the Bot API method a reply file is sent with. It comes from the
// engine's reply_parts `send_as`; render never derives it from the path.
// The zero value is MethodUnset, so a File built without a method is caught
// (see PlanReply) instead of going out as a document.
type Method int

const (
	MethodUnset    Method = iota // no send_as: the item fails, nothing is sent
	MethodDocument               // sendDocument
	MethodPhoto                  // sendPhoto
	MethodVoice                  // sendVoice
	MethodAudio                  // sendAudio
)

func (m Method) String() string {
	switch m {
	case MethodDocument:
		return "sendDocument"
	case MethodPhoto:
		return "sendPhoto"
	case MethodVoice:
		return "sendVoice"
	case MethodAudio:
		return "sendAudio"
	default:
		return "unset"
	}
}

// MethodFromSendAs maps a reply_parts `send_as` value. An unknown value is
// an error, never a document: a newer engine may add a method this gateway
// cannot send.
func MethodFromSendAs(sendAs string) (Method, error) {
	switch sendAs {
	case "document":
		return MethodDocument, nil
	case "photo":
		return MethodPhoto, nil
	case "voice":
		return MethodVoice, nil
	case "audio":
		return MethodAudio, nil
	}
	return MethodUnset, fmt.Errorf("unknown send_as %q", sendAs)
}

// valid reports a method PlanReply can send.
func (m Method) valid() bool { return m >= MethodDocument && m <= MethodAudio }

// takesReplyCaption reports whether a lone reply file may carry the reply
// text as its caption: photos, voice notes and audio. A document always goes
// out on its own, after the text.
func (m Method) takesReplyCaption() bool {
	return m == MethodPhoto || m == MethodVoice || m == MethodAudio
}
