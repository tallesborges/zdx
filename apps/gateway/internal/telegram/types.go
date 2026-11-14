package telegram

import (
	"encoding/json"
	"strings"
)

// Update is one getUpdates item. Only message and callback_query are
// requested.
type Update struct {
	ID            int64          `json:"update_id"`
	Message       *Message       `json:"message,omitempty"`
	CallbackQuery *CallbackQuery `json:"callback_query,omitempty"`
}

// CallbackQuery is a press of an inline keyboard button.
type CallbackQuery struct {
	ID      string   `json:"id"`
	From    User     `json:"from"`
	Message *Message `json:"message,omitempty"`
	Data    *string  `json:"data,omitempty"`
}

// Message is an incoming Telegram message. Fields the gateway does not model
// stay available in Raw.
type Message struct {
	ID             int64           `json:"message_id"`
	Chat           Chat            `json:"chat"`
	From           *User           `json:"from,omitempty"`
	Date           int64           `json:"date"`
	MediaGroupID   *string         `json:"media_group_id,omitempty"`
	Text           *string         `json:"text,omitempty"`
	RichMessage    json.RawMessage `json:"rich_message,omitempty"`
	Caption        *string         `json:"caption,omitempty"`
	Photo          []PhotoSize     `json:"photo,omitempty"`
	Voice          *File           `json:"voice,omitempty"`
	Audio          *File           `json:"audio,omitempty"`
	Document       *File           `json:"document,omitempty"`
	Video          *File           `json:"video,omitempty"`
	VideoNote      *File           `json:"video_note,omitempty"`
	Animation      *File           `json:"animation,omitempty"`
	PaidMedia      json.RawMessage `json:"paid_media,omitempty"`
	ThreadID       *int64          `json:"message_thread_id,omitempty"`
	ReplyTo        *Message        `json:"reply_to_message,omitempty"`
	Quote          *TextQuote      `json:"quote,omitempty"`
	ReplyMarkup    json.RawMessage `json:"reply_markup,omitempty"`
	IsTopicMessage bool            `json:"is_topic_message,omitempty"`

	Raw json.RawMessage `json:"-"`
}

func (m *Message) UnmarshalJSON(data []byte) error {
	type plain Message
	var p plain
	if err := json.Unmarshal(data, &p); err != nil {
		return err
	}
	*m = Message(p)
	m.Raw = append(json.RawMessage(nil), data...)
	return nil
}

// EffectiveThreadID is the topic of the message, or of the message it
// replies to.
func (m *Message) EffectiveThreadID() *int64 {
	if m.ThreadID != nil {
		return m.ThreadID
	}
	if m.ReplyTo != nil {
		return m.ReplyTo.ThreadID
	}
	return nil
}

// TextQuote is the quoted part of a replied-to message.
type TextQuote struct {
	Text string `json:"text"`
}

// Chat is the chat of a message.
type Chat struct {
	ID      int64  `json:"id"`
	Type    string `json:"type"`
	IsForum *bool  `json:"is_forum,omitempty"`
}

func (c Chat) IsPrivate() bool { return c.Type == "private" }
func (c Chat) IsGroup() bool   { return c.Type == "group" || c.Type == "supergroup" }
func (c Chat) IsForumEnabled() bool {
	return c.IsForum != nil && *c.IsForum
}

// User is a message or callback sender.
type User struct {
	ID    int64 `json:"id"`
	IsBot bool  `json:"is_bot"`
}

type PhotoSize struct {
	FileID   string  `json:"file_id"`
	Width    int64   `json:"width"`
	Height   int64   `json:"height"`
	FileSize *uint64 `json:"file_size,omitempty"`
}

// File covers voice, audio, document, video, video note and animation.
type File struct {
	FileID   string  `json:"file_id"`
	FileName *string `json:"file_name,omitempty"`
	MimeType *string `json:"mime_type,omitempty"`
	FileSize *uint64 `json:"file_size,omitempty"`
}

// RemoteFile is a getFile result.
type RemoteFile struct {
	FilePath *string `json:"file_path,omitempty"`
}

// InlineKeyboardMarkup is an inline keyboard. An empty keyboard removes the
// buttons on edit (omitting reply_markup keeps them).
type InlineKeyboardMarkup struct {
	InlineKeyboard [][]InlineKeyboardButton `json:"inline_keyboard"`
}

// EmptyKeyboard returns a keyboard that removes buttons on edit.
func EmptyKeyboard() *InlineKeyboardMarkup {
	return &InlineKeyboardMarkup{InlineKeyboard: [][]InlineKeyboardButton{}}
}

type InlineKeyboardButton struct {
	Text         string      `json:"text"`
	CallbackData *string     `json:"callback_data,omitempty"`
	URL          *string     `json:"url,omitempty"`
	WebApp       *WebAppInfo `json:"web_app,omitempty"`
}

type WebAppInfo struct {
	URL string `json:"url"`
}

// CallbackButton returns a button with callback data.
func CallbackButton(text, data string) InlineKeyboardButton {
	return InlineKeyboardButton{Text: text, CallbackData: &data}
}

// URLButton returns a link button.
func URLButton(text, url string) InlineKeyboardButton {
	return InlineKeyboardButton{Text: text, URL: &url}
}

// ReplyParameters replies to a message, possibly in another chat or topic.
type ReplyParameters struct {
	MessageID                int64  `json:"message_id"`
	ChatID                   *int64 `json:"chat_id,omitempty"`
	AllowSendingWithoutReply *bool  `json:"allow_sending_without_reply,omitempty"`
}

// BotCommand is one entry of the command menu.
type BotCommand struct {
	Command     string `json:"command"`
	Description string `json:"description"`
}

// CommandMatches reports whether trimmed text is exactly command, or command
// addressed to a bot (`/cmd@bot`).
func CommandMatches(text, command string) bool {
	t := strings.TrimSpace(text)
	if t == command {
		return true
	}
	rest, ok := strings.CutPrefix(t, command)
	return ok && strings.HasPrefix(rest, "@")
}
