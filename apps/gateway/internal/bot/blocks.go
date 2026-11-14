package bot

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"

	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// TopicBlocks are launcher orchestrator topics whose setup failed: such a
// topic has no profile, so a turn there would run the default coding agent.
// A block holds in memory and as an empty marker file
// <Dir>/<thread id> ($ZDX_HOME/telegram/unsetup-topics, the Rust bot's
// layout, a write Go owns), so it survives a restart. Either one refuses a
// turn.
type TopicBlocks struct {
	Dir string

	mu sync.Mutex
	m  map[string]bool
}

// BlockTopic blocks threadID. The in-memory block always holds; an error
// means the marker was not written, so the block lasts until restart only.
func (b *TopicBlocks) BlockTopic(_ context.Context, threadID string) error {
	b.mu.Lock()
	if b.m == nil {
		b.m = map[string]bool{}
	}
	b.m[threadID] = true
	b.mu.Unlock()
	marker, err := b.marker(threadID)
	if err != nil {
		return err
	}
	if err := sandbox.CheckHome(b.Dir); err != nil {
		return err
	}
	if err := os.MkdirAll(b.Dir, 0o777); err != nil {
		return err
	}
	return os.WriteFile(marker, nil, 0o666)
}

// Blocked reports a blocked topic thread.
func (b *TopicBlocks) Blocked(threadID string) bool {
	b.mu.Lock()
	blocked := b.m[threadID]
	b.mu.Unlock()
	if blocked {
		return true
	}
	marker, err := b.marker(threadID)
	if err != nil {
		return false
	}
	_, err = os.Stat(marker)
	return err == nil
}

func (b *TopicBlocks) marker(threadID string) (string, error) {
	if !filepath.IsAbs(b.Dir) {
		return "", errors.New("topic block directory must be absolute")
	}
	if threadID == "" || threadID == "." || threadID == ".." || filepath.Base(threadID) != threadID {
		return "", errors.New("invalid thread id for a topic block")
	}
	return filepath.Join(b.Dir, threadID), nil
}

// UnsetupTopicNotice answers a message in a blocked topic
// (commands.UnsetupTopicNotice, the Rust bot's text).
const UnsetupTopicNotice = "⚠️ I couldn't set up this topic as an orchestrator on the picked model, so I won't run messages here. Open a new one from /launcher."
