package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Settings is the resolved Telegram runtime, as printed by
// `zdx telegram settings --include-token`. Config layering, env
// fallbacks and profile resolution stay in Rust. Read it from a pipe; the
// token is never logged.
type Settings struct {
	BotToken         string  `json:"bot_token"`
	AllowlistUserIDs []int64 `json:"allowlist_user_ids"`
	// AllowlistChatIDs includes every profile chat, as Rust resolves it.
	AllowlistChatIDs []int64 `json:"allowlist_chat_ids"`
	Server           struct {
		Enabled    bool    `json:"enabled"`
		Port       uint16  `json:"port"`
		MiniAppURL *string `json:"mini_app_url,omitempty"`
	} `json:"server"`
	// Root is the bot's default project root.
	Root     string    `json:"root"`
	Profiles []Profile `json:"profiles"`
	// ConfigLayers are the config files the profiles come from (Rust
	// watched_config_layers), listed whether or not they exist.
	ConfigLayers []string `json:"config_layers,omitempty"`
	// Watch, when set, holds the current profiles: every copy of these
	// settings reads them through it (RootForChat, chat bindings).
	Watch *SettingsWatch `json:"-"`
}

// profiles are the current chat bindings.
func (s Settings) profiles() []Profile {
	if s.Watch != nil {
		if p := s.Watch.profiles.Load(); p != nil {
			return *p
		}
	}
	return s.Profiles
}

// Profile is the current binding of chat, if any.
func (s Settings) Profile(chat int64) (Profile, bool) {
	for _, p := range s.profiles() {
		if p.ChatID == chat {
			return p, true
		}
	}
	return Profile{}, false
}

// Refresh picks up profile edits since the last call (no-op without a
// Watch).
func (s Settings) Refresh(ctx context.Context) { s.Watch.Refresh(ctx) }

// Profile binds a chat to a project folder.
type Profile struct {
	Name         string  `json:"name"`
	ChatID       int64   `json:"chat_id"`
	Cwd          string  `json:"cwd"`
	Orchestrator bool    `json:"orchestrator"`
	WorkerRoot   *string `json:"worker_root,omitempty"`
}

// RootForChat is the project root a chat's turns submit with: its
// profile's folder, else the default root (BotContext::root_for_chat).
func (s Settings) RootForChat(chat int64) string {
	if p, ok := s.Profile(chat); ok {
		return p.Cwd
	}
	return s.Root
}

// SettingsWatch follows profile edits as the Rust bot does before each turn
// (BotContext::reload_config_if_changed): Refresh stats the config layers
// and loads the settings again only when one changed. Only the profiles
// change; the token, allowlists, server and root stay as the process
// started, as in Rust. A failed load keeps the last good profiles and is
// not tried again until a layer changes again.
type SettingsWatch struct {
	// Load reads the settings again (`zdx telegram settings`).
	Load func(ctx context.Context) (Settings, error)
	Log  *slog.Logger

	mu       sync.Mutex
	profiles atomic.Pointer[[]Profile]
	layers   []string
	stamps   []layerStamp
}

// layerStamp is what Refresh compares: a missing file, or its size and time.
type layerStamp struct {
	exists bool
	size   int64
	mod    time.Time
}

func stampLayers(paths []string) []layerStamp {
	out := make([]layerStamp, len(paths))
	for i, p := range paths {
		if fi, err := os.Stat(p); err == nil {
			out[i] = layerStamp{exists: true, size: fi.Size(), mod: fi.ModTime()}
		}
	}
	return out
}

// WatchSettings starts following the profiles of s, which load produced.
// It returns s with Watch set.
func WatchSettings(s Settings, load func(ctx context.Context) (Settings, error), log *slog.Logger) Settings {
	w := &SettingsWatch{Load: load, Log: log, layers: s.ConfigLayers, stamps: stampLayers(s.ConfigLayers)}
	profiles := s.Profiles
	w.profiles.Store(&profiles)
	s.Watch = w
	return s
}

// SettingsReloadTimeout bounds one settings load in Refresh.
const SettingsReloadTimeout = 30 * time.Second

// Refresh loads the settings again when a config layer changed. Callers
// wait for a load in progress, so work admitted after an edit sees it.
func (w *SettingsWatch) Refresh(ctx context.Context) {
	if w == nil || w.Load == nil {
		return
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	before := stampLayers(w.layers)
	if slices.Equal(before, w.stamps) {
		return
	}
	log := w.Log
	if log == nil {
		log = slog.Default()
	}
	lctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), SettingsReloadTimeout)
	next, err := w.Load(lctx)
	cancel()
	if err != nil {
		w.stamps = before
		log.Warn("config changed but the settings did not load; keeping the previous profiles", "err", err)
		return
	}
	profiles := next.Profiles
	w.profiles.Store(&profiles)
	if slices.Equal(next.ConfigLayers, w.layers) {
		// An edit during the load changes the stamp again.
		w.stamps = before
	} else {
		w.layers = next.ConfigLayers
		w.stamps = stampLayers(w.layers)
	}
	log.Info("reloaded profile bindings after a config change", "profiles", len(profiles))
}

// MiniAppURL is the Mini App base URL for links, when the server is enabled.
func (s Settings) MiniAppURL() string {
	if !s.Server.Enabled || s.Server.MiniAppURL == nil {
		return ""
	}
	return *s.Server.MiniAppURL
}

// LogValue keeps the token out of structured logs, text and JSON handlers.
func (s Settings) LogValue() slog.Value {
	return slog.StringValue(s.String())
}

// GoString keeps the token out of %#v.
func (s Settings) GoString() string { return s.String() }

// MarshalJSON writes the settings without the token, so no encoder can
// leak it. Settings are read with ReadSettings, never written back.
func (s Settings) MarshalJSON() ([]byte, error) {
	type plain Settings
	p := plain(s)
	p.BotToken = telegram.BotIdentity(s.BotToken) + ":<redacted>"
	return json.Marshal(p)
}

// String never prints the token.
func (s Settings) String() string {
	return fmt.Sprintf("Settings{bot=%s users=%d chats=%d server=%v:%d}",
		telegram.BotIdentity(s.BotToken), len(s.AllowlistUserIDs), len(s.AllowlistChatIDs), s.Server.Enabled, s.Server.Port)
}

// ReadSettings decodes and validates settings JSON.
func ReadSettings(r io.Reader) (Settings, error) {
	var s Settings
	dec := json.NewDecoder(r)
	dec.DisallowUnknownFields()
	if err := dec.Decode(&s); err != nil {
		return Settings{}, fmt.Errorf("read gateway settings: %w", err)
	}
	if strings.Contains(s.BotToken, "<redacted>") {
		return Settings{}, errors.New("gateway settings: bot_token is redacted; read them from `zdx telegram settings --include-token`, not from logged or encoded settings")
	}
	if s.BotToken == "" {
		return Settings{}, errors.New("gateway settings: bot_token is required (run `zdx telegram settings --include-token`)")
	}
	if s.Root == "" {
		return Settings{}, errors.New("gateway settings: root is required")
	}
	if len(s.AllowlistUserIDs) == 0 {
		return Settings{}, errors.New("gateway settings: allowlist_user_ids must contain at least one user ID")
	}
	if s.Server.Enabled && s.Server.Port == 0 {
		return Settings{}, errors.New("gateway settings: server.port is required when the server is enabled")
	}
	return s, nil
}
