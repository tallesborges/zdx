// Package conversation maps Telegram chats and topics to the thread ids the
// bot mints, and back. Same rules as zdx_engine::telegram's
// conversation_thread_id / parse_topic_conversation_id and the bot's
// artifact_feedback::route_for_thread.
package conversation

import (
	"strconv"
	"strings"
)

// Route is a Telegram chat and optional forum topic.
type Route struct {
	Chat  int64
	Topic *int64
}

// ThreadID is `telegram-<chat>-topic-<topic>`, or `telegram-<chat>` without
// a topic.
func ThreadID(chat int64, topic *int64) string {
	if topic != nil {
		return "telegram-" + strconv.FormatInt(chat, 10) + "-topic-" + strconv.FormatInt(*topic, 10)
	}
	return "telegram-" + strconv.FormatInt(chat, 10)
}

// Parse returns the route of a bot-minted thread id. The topic form splits
// at the last `-topic-`; anything else must be `telegram-<chat>`.
func Parse(threadID string) (Route, bool) {
	rest, ok := strings.CutPrefix(threadID, "telegram-")
	if !ok {
		return Route{}, false
	}
	if i := strings.LastIndex(rest, "-topic-"); i >= 0 {
		chat, chatErr := strconv.ParseInt(rest[:i], 10, 64)
		topic, topicErr := strconv.ParseInt(rest[i+len("-topic-"):], 10, 64)
		if chatErr == nil && topicErr == nil {
			return Route{Chat: chat, Topic: &topic}, true
		}
	}
	chat, err := strconv.ParseInt(rest, 10, 64)
	if err != nil {
		return Route{}, false
	}
	return Route{Chat: chat}, true
}
