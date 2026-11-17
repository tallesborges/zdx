// Package ui is the chat TUI's Bubble Tea model. It renders a thread with
// the shared transcript layout and runs every turn through the engine
// service: it owns presentation, composition, and key handling, never an
// engine rule.
package ui

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/transcript"
	"github.com/tallesborges/zdx/apps/tui/internal/engine"
)

var (
	title    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Cyan)
	selected = lipgloss.NewStyle().Reverse(true)
	rule     = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(240))
)

// reconnectDelay is the wait before subscribing again after the service
// connection ended.
const reconnectDelay = 2 * time.Second

// Options configure a chat.
type Options struct {
	// Root is the project root: config layers load from it, and a new
	// thread runs there.
	Root string
	// Home is the ZDX home (config file for /config, ZDX_HOME of /login).
	Home string
	// Zdx is the zdx binary for /login; empty disables it.
	Zdx string
	// ThreadID resumes a thread; empty starts a new one.
	ThreadID string
	// Model is a model spec or `mode:<name>` to set on the thread at start.
	Model string
}

// Backend is what the model needs from the service; *engine.Client
// implements it, tests pass fakes.
type Backend interface {
	Require(ctx context.Context, caps ...string) error
	Submit(ctx context.Context, req engine.SubmitRequest) (engine.Submitted, error)
	Subscribe(ctx context.Context, threadID string) (*engine.Subscription, error)
	Cancel(ctx context.Context, threadID string, clearQueue bool) error
	UpdateThread(ctx context.Context, u engine.Update) (engine.Updated, error)
	GenerateTitle(ctx context.Context, threadID, text string) (string, error)
	Picker(ctx context.Context, threadID string) (engine.Picker, error)
	Status(ctx context.Context, threadID string) (engine.ThreadStatus, error)
	Threads(ctx context.Context) ([]transcript.ThreadListItem, error)
	Thread(ctx context.Context, id string, after *uint64) (transcript.Thread, error)
	PrepareImage(ctx context.Context, path string) (*engine.Image, error)
	Followups(ctx context.Context, finalText string) ([]string, error)
	RunningTurn(ctx context.Context, threadID string) (uint64, error)
	TranscribeAudio(ctx context.Context, path, mime string) (string, error)
}

type attachment struct {
	name  string
	image engine.Image
}

// outItem is a message sent and not yet part of a turn: waiting for the
// service's receipt, queued, or a steer the running turn has not read.
type outItem struct {
	local   uint64
	inputID uint64
	acked   bool
	text    string
	images  int
	steer   bool
}

// Model is the chat screen.
type Model struct {
	ctx  context.Context
	be   Backend
	opts Options

	width, height int

	threadID string
	// gen changes whenever the shown thread changes; answers for an
	// older one are dropped.
	gen uint64
	// threadCtx ends with the shown thread (its subscription, its loads).
	threadCtx    context.Context
	threadCancel context.CancelFunc
	sub          *engine.Subscription
	reconnecting bool

	// saved is false until the thread has its first message on disk.
	saved  bool
	title  string
	thread *transcript.Thread
	// loadSeq numbers thread loads; an answer older than the last applied
	// one is dropped.
	loadSeq, appliedSeq uint64
	md                  transcript.Cache
	liveMD              transcript.Cache

	// turns are the turns seen on the subscription and not yet replaced by
	// the saved thread, oldest first.
	turns []*live
	// turnID is the running turn (0 when none).
	turnID uint64
	outbox []outItem
	// early are inputs whose turn start, steer read, or drop came before
	// their submit receipt: input id → turn id (0: injected or dropped).
	early     map[uint64]uint64
	nextLocal uint64
	pending   int

	failed bool
	// followups are the last reply's suggested replies (ctrl+f).
	followups  []string
	firstSent  bool
	pendingSel *string

	status   engine.ThreadStatus
	hasStat  bool
	notice   string
	noticeOK bool

	composer    textarea.Model
	attachments []attachment
	history     []string
	histPos     int
	draft       string

	overlay  *picker
	scroll   int
	thinking bool

	rec        *recorder
	transcribe bool

	lines      []string
	linesKey   string
	liveDirty  bool
	cachedLive []string

	// mouse drag selection over the transcript viewport.
	sel     *transcript.Range
	selDrag bool
	// selLines are the plain lines the selection maps to: the viewport
	// body before highlight, in screen order.
	selLines []string

	// mouse hover over the transcript viewport (a data line, or -1),
	// and the open tool detail (nil when closed).
	hoverLine    int
	detail       *transcript.ToolRef
	detailScroll int
	detailLines  []string
}

// New returns a chat model.
func New(ctx context.Context, be Backend, opts Options) *Model {
	ta := textarea.New()
	ta.Prompt = "› "
	ta.Placeholder = "Message  (enter send · shift+enter newline · ctrl+o commands)"
	ta.ShowLineNumbers = false
	ta.CharLimit = 0
	ta.DynamicHeight = true
	ta.MinHeight = 1
	ta.MaxHeight = 8
	ta.MaxContentHeight = 100000
	ta.KeyMap.InsertNewline = key.NewBinding(key.WithKeys("shift+enter", "alt+enter", "ctrl+j"))
	ta.KeyMap.LineNext = key.NewBinding(key.WithKeys("down"))
	ta.KeyMap.LinePrevious = key.NewBinding(key.WithKeys("up"))
	ta.KeyMap.TransposeCharacterBackward = key.NewBinding()
	ta.KeyMap.PageUp = key.NewBinding()
	ta.KeyMap.PageDown = key.NewBinding()
	ta.KeyMap.DeleteCharacterForward = key.NewBinding(key.WithKeys("delete"))
	ta.Focus()
	m := &Model{ctx: ctx, be: be, opts: opts, composer: ta, histPos: -1, early: map[uint64]uint64{}, hoverLine: -1}
	if opts.ThreadID != "" {
		m.threadID, m.saved = opts.ThreadID, true
	} else {
		m.threadID = newThreadID()
	}
	m.threadCtx, m.threadCancel = context.WithCancel(ctx)
	return m
}

func newThreadID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

// Messages.
type (
	readyMsg  struct{ err error }
	threadMsg struct {
		gen, seq uint64
		thread   transcript.Thread
		after    *uint64
		// ended are the turns that had ended when the load was asked: the
		// answer has them saved.
		ended []uint64
		err   error
	}
	statusMsg struct {
		gen    uint64
		status engine.ThreadStatus
		err    error
	}
	subscribedMsg struct {
		gen uint64
		sub *engine.Subscription
		err error
	}
	resubscribeMsg struct{ gen uint64 }
	runningMsg     struct {
		gen, turn uint64
	}
	eventMsg struct {
		gen uint64
		sub *engine.Subscription
		ev  engine.Event
		ok  bool
	}
	submittedMsg struct {
		gen   uint64
		local uint64
		sub   engine.Submitted
		err   error
	}
	noticeMsg struct {
		text string
		ok   bool
	}
	titledMsg struct {
		gen   uint64
		title string
	}
	followupsMsg struct {
		gen, sent uint64
		items     []string
	}
	pickerMsg struct {
		p    *picker
		data pickerData
		err  error
	}
	imageMsg struct {
		name  string
		image *engine.Image
		err   error
	}
	transcribedMsg struct {
		text string
		err  error
	}
	execDoneMsg struct {
		what string
		err  error
	}
)

// Init checks the service, subscribes to the thread, loads it, and applies
// --model.
func (m *Model) Init() tea.Cmd {
	cmds := []tea.Cmd{textarea.Blink, m.checkReady(), m.subscribe(), m.loadThread(nil), m.loadStatus()}
	if m.opts.Model != "" {
		cmds = append(cmds, m.setModel(m.opts.Model))
	}
	return tea.Batch(cmds...)
}

// Close stops what outlives a frame: the recorder and the thread's
// connections. The program calls it after Run returns.
func (m *Model) Close() {
	if m.rec != nil {
		m.rec.discard()
		m.rec = nil
	}
	m.threadCancel()
}

func (m *Model) quit() tea.Cmd {
	m.Close()
	return tea.Quit
}

func (m *Model) checkReady() tea.Cmd {
	ctx, be := m.ctx, m.be
	return func() tea.Msg { return readyMsg{err: be.Require(ctx, engine.Required...)} }
}

func (m *Model) subscribe() tea.Cmd {
	ctx, be, id, gen := m.threadCtx, m.be, m.threadID, m.gen
	return func() tea.Msg {
		sub, err := be.Subscribe(ctx, id)
		return subscribedMsg{gen: gen, sub: sub, err: err}
	}
}

func (m *Model) loadThread(after *uint64) tea.Cmd {
	if !m.saved {
		return nil
	}
	m.loadSeq++
	var ended []uint64
	for _, t := range m.turns {
		if t.ended {
			ended = append(ended, t.turn)
		}
	}
	ctx, be, id, gen, seq := m.threadCtx, m.be, m.threadID, m.gen, m.loadSeq
	return func() tea.Msg {
		th, err := be.Thread(ctx, id, after)
		return threadMsg{gen: gen, seq: seq, thread: th, after: after, ended: ended, err: err}
	}
}

func (m *Model) loadStatus() tea.Cmd {
	ctx, be, id, gen := m.threadCtx, m.be, m.threadID, m.gen
	return func() tea.Msg {
		s, err := be.Status(ctx, id)
		return statusMsg{gen: gen, status: s, err: err}
	}
}

func waitEvent(gen uint64, sub *engine.Subscription) tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-sub.Events
		return eventMsg{gen: gen, sub: sub, ev: ev, ok: ok}
	}
}

func (m *Model) setNotice(text string, ok bool) { m.notice, m.noticeOK = text, ok }

func errText(err error) string {
	var se *protocol.ServiceError
	if errors.As(err, &se) && se.Code != "" {
		return se.Code + ": " + se.Message
	}
	return err.Error()
}

// Update handles one message.
func (m *Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
		m.composer.SetWidth(max(m.width-2, 10))
		if m.detail != nil {
			m.detailLines = transcript.Detail(m.fullThread(), m.detail.ID, m.width-1)
		}
		return m, nil
	case readyMsg:
		if msg.err != nil {
			m.setNotice(errText(msg.err), false)
		}
		return m, nil
	case subscribedMsg:
		return m, m.applySubscribed(msg)
	case runningMsg:
		// A turn that started before the subscription: steer and interrupt
		// apply to it until its turn_ended arrives.
		if msg.gen == m.gen && msg.turn != 0 && m.turnID == 0 {
			if t := m.findTurn(msg.turn); t == nil || !t.ended {
				m.turnID = msg.turn
			}
		}
		return m, nil
	case resubscribeMsg:
		if msg.gen != m.gen {
			return m, nil
		}
		return m, m.subscribe()
	case threadMsg:
		return m, m.applyThread(msg)
	case statusMsg:
		if msg.gen == m.gen && msg.err == nil {
			transcript.CleanAll(&msg.status)
			m.status, m.hasStat = msg.status, true
			if m.pendingSel != nil {
				m.status.Model, m.status.Thinking = *m.pendingSel, ""
			}
		}
		return m, nil
	case submittedMsg:
		return m, m.applySubmitted(msg)
	case eventMsg:
		return m, m.applyEvent(msg)
	case noticeMsg:
		m.setNotice(msg.text, msg.ok)
		return m, nil
	case followupsMsg:
		// Dropped once another message went out: they answered the last.
		if msg.gen == m.gen && msg.sent == m.nextLocal {
			m.followups = nil
			for _, f := range msg.items {
				if f = transcript.OneLine(f); f != "" {
					m.followups = append(m.followups, f)
				}
			}
			m.liveDirty = true
		}
		return m, nil
	case titledMsg:
		if msg.gen == m.gen && msg.title != "" {
			m.title = transcript.OneLine(msg.title)
		}
		return m, nil
	case pickerMsg:
		if m.overlay != nil && m.overlay == msg.p {
			m.overlay.install(msg.data, msg.err)
		}
		return m, nil
	case imageMsg:
		if msg.err != nil {
			m.setNotice("image "+msg.name+": "+errText(msg.err), false)
		} else {
			m.attachments = append(m.attachments, attachment{name: msg.name, image: *msg.image})
			m.setNotice("attached "+msg.name, true)
		}
		return m, nil
	case transcribedMsg:
		m.transcribe = false
		switch {
		case msg.err != nil:
			m.setNotice("voice: "+errText(msg.err), false)
		case strings.TrimSpace(msg.text) == "":
			m.setNotice("no transcript: is transcription configured?", false)
		default:
			m.composer.InsertString(strings.TrimSpace(msg.text))
			m.setNotice("", true)
		}
		return m, nil
	case execDoneMsg:
		if msg.err != nil {
			m.setNotice(msg.what+": "+msg.err.Error(), false)
		} else {
			m.setNotice(msg.what+" done", true)
		}
		return m, m.loadStatus()
	case tea.PasteMsg:
		if path, ok := imagePath(msg.Content); ok {
			return m, m.attachImage(path)
		}
	case tea.MouseWheelMsg:
		if m.overlay != nil {
			return m, nil
		}
		if m.detail != nil {
			switch msg.Button {
			case tea.MouseWheelUp:
				m.detailScroll--
			case tea.MouseWheelDown:
				m.detailScroll++
			}
			return m, nil
		}
		switch msg.Button {
		case tea.MouseWheelUp:
			m.scrollBy(3)
		case tea.MouseWheelDown:
			m.scrollBy(-3)
		}
		return m, nil
	case tea.MouseClickMsg:
		return m, m.mouseClick(msg.Mouse())
	case tea.MouseMotionMsg:
		return m, m.mouseMotion(msg.Mouse())
	case tea.MouseReleaseMsg:
		return m, m.mouseRelease(msg.Mouse())
	case tea.KeyPressMsg:
		if cmd, handled := m.handleKey(msg); handled {
			return m, cmd
		}
	}
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	return m, cmd
}

func (m *Model) applySubscribed(msg subscribedMsg) tea.Cmd {
	if msg.gen != m.gen {
		return nil
	}
	if msg.err != nil {
		m.setNotice("engine: "+errText(msg.err)+" (retrying)", false)
		return m.resubscribeLater()
	}
	m.sub = msg.sub
	cmds := []tea.Cmd{waitEvent(m.gen, msg.sub), m.loadRunning()}
	if m.reconnecting {
		// Events may have passed while disconnected, and a restarted
		// service numbers inputs anew: the saved thread is the truth again,
		// and nothing sent before is waited for (it is in the thread if it
		// ran; nothing is re-sent).
		m.reconnecting = false
		m.turns, m.turnID, m.outbox, m.liveDirty = nil, 0, nil, true
		m.early = map[uint64]uint64{}
		m.setNotice("reconnected", true)
		m.thread = nil
		cmds = append(cmds, m.loadThread(nil), m.loadStatus())
	}
	return tea.Batch(cmds...)
}

func (m *Model) loadRunning() tea.Cmd {
	ctx, be, id, gen := m.threadCtx, m.be, m.threadID, m.gen
	return func() tea.Msg {
		turn, err := be.RunningTurn(ctx, id)
		if err != nil {
			return nil
		}
		return runningMsg{gen: gen, turn: turn}
	}
}

func (m *Model) resubscribeLater() tea.Cmd {
	gen := m.gen
	return tea.Tick(reconnectDelay, func(time.Time) tea.Msg { return resubscribeMsg{gen: gen} })
}

func (m *Model) applyThread(msg threadMsg) tea.Cmd {
	if msg.gen != m.gen || msg.seq <= m.appliedSeq {
		return nil
	}
	if msg.err != nil {
		if engine.IsNotFound(msg.err) && msg.after == nil {
			// A resumed id that has no file yet is a new thread.
			m.saved = false
			return nil
		}
		m.setNotice("thread: "+errText(msg.err), false)
		return nil
	}
	if msg.after != nil && !m.quiet() {
		// The file may already hold part of a turn that is still live
		// here: showing both would repeat it. The load after the last
		// turn ends installs it.
		return nil
	}
	m.appliedSeq = msg.seq
	transcript.CleanAll(&msg.thread)
	old := m.thread
	if msg.after != nil && old != nil && msg.thread.Partial &&
		(msg.thread.Cursor < *msg.after || msg.thread.SourceID != old.SourceID) {
		// Reset or moved alias: load it all again.
		m.thread = nil
		return m.loadThread(nil)
	}
	merged := transcript.Merge(old, msg.thread)
	m.thread = &merged
	if merged.Title != "" {
		m.title = merged.Title
	}
	m.linesKey = ""
	// The saved thread now has these turns.
	ended := map[uint64]bool{}
	for _, id := range msg.ended {
		ended[id] = true
	}
	kept := m.turns[:0]
	for _, t := range m.turns {
		if !ended[t.turn] {
			kept = append(kept, t)
		}
	}
	m.turns = kept
	m.liveDirty = true
	if len(m.history) == 0 {
		for _, a := range merged.Activity {
			if a.Type == "message" && a.Role == "user" && strings.TrimSpace(a.Text) != "" {
				m.history = append(m.history, a.Text)
			}
		}
	}
	return nil
}

// quiet reports no live turn still running and no message the service
// queued: the saved thread then has everything the screen shows live.
func (m *Model) quiet() bool {
	for _, t := range m.turns {
		if !t.ended {
			return false
		}
	}
	for _, o := range m.outbox {
		if o.acked {
			return false
		}
	}
	return m.turnID == 0
}

func (m *Model) outIndex(match func(*outItem) bool) int {
	for i := range m.outbox {
		if match(&m.outbox[i]) {
			return i
		}
	}
	return -1
}

func (m *Model) dropOut(i int) {
	m.outbox = append(m.outbox[:i], m.outbox[i+1:]...)
	m.liveDirty = true
}

func (m *Model) applySubmitted(msg submittedMsg) tea.Cmd {
	if msg.gen != m.gen {
		return nil
	}
	m.pending--
	i := m.outIndex(func(o *outItem) bool { return o.local == msg.local })
	if i < 0 {
		return nil
	}
	item := m.outbox[i]
	if msg.err != nil {
		m.dropOut(i)
		m.setNotice("send: "+errText(msg.err), false)
		if m.composer.Value() == "" && item.text != "" {
			// Nothing reached the thread: give the text back.
			m.composer.SetValue(item.text)
		}
		return nil
	}
	sub := msg.sub
	wasSaved := m.saved
	m.saved = true
	var cmds []tea.Cmd
	if !m.firstSent {
		m.firstSent = true
		if m.title == "" && strings.TrimSpace(item.text) != "" {
			cmds = append(cmds, m.autoTitle(item.text))
		}
	}
	m.outbox[i].inputID, m.outbox[i].acked = sub.InputID, true
	m.outbox[i].steer = sub.InTurn
	if turn, seen := m.early[sub.InputID]; seen {
		// The subscription was faster than the receipt.
		delete(m.early, sub.InputID)
		m.dropOut(i)
		if t := m.findTurn(turn); t != nil && turn != 0 {
			t.prependUser(item.text, item.images)
			if t.ended && !wasSaved {
				// Its load was skipped while the thread had no file.
				cmds = append(cmds, m.loadThread(nil), m.loadStatus())
			}
		}
	}
	if m.pending == 0 {
		// Starts of inputs no receipt here waits for (other clients').
		m.early = map[uint64]uint64{}
	}
	switch {
	case sub.InTurn:
		m.setNotice("steer sent; it joins the turn after the current tool calls", true)
	case sub.QueuedAhead > 0:
		m.setNotice(fmt.Sprintf("queued (%d ahead)", sub.QueuedAhead), true)
	}
	m.liveDirty = true
	return tea.Batch(cmds...)
}

func (m *Model) findTurn(id uint64) *live {
	for _, t := range m.turns {
		if t.turn == id {
			return t
		}
	}
	return nil
}

// turnFor is the live state of a turn, made when the subscription joined
// it after its start.
func (m *Model) turnFor(id uint64) *live {
	if t := m.findTurn(id); t != nil {
		return t
	}
	t := newLive(id)
	m.turns = append(m.turns, t)
	return t
}

func (m *Model) applyEvent(msg eventMsg) tea.Cmd {
	if msg.gen != m.gen || msg.sub != m.sub {
		return nil
	}
	if !msg.ok {
		err := msg.sub.Err()
		m.sub = nil
		if m.threadCtx.Err() != nil {
			return nil
		}
		text := "lost the engine service connection; reconnecting"
		if err != nil {
			text += " (" + errText(err) + ")"
		}
		m.setNotice(text, false)
		m.reconnecting = true
		return m.resubscribeLater()
	}
	next := waitEvent(m.gen, msg.sub)
	ev := msg.ev
	switch ev.Type {
	case engine.EventTurnStarted:
		t := m.turnFor(ev.TurnID)
		m.turnID = ev.TurnID
		m.failed = false
		for _, in := range ev.Inputs {
			if i := m.outIndex(func(o *outItem) bool { return o.acked && o.inputID == in.InputID }); i >= 0 {
				t.user(m.outbox[i].text, m.outbox[i].images)
				m.dropOut(i)
			} else if m.pending > 0 {
				m.early[in.InputID] = ev.TurnID
			}
		}
		m.liveDirty = true
	case engine.EventAgent:
		a, err := ev.Agent()
		if err != nil {
			m.setNotice("undecodable event: "+err.Error(), false)
			return next
		}
		t := m.turnFor(ev.TurnID)
		if m.turnID == 0 && !t.ended && a.Type != "turn_finished" {
			// Joined after its start.
			m.turnID = ev.TurnID
		}
		switch a.Type {
		case "provider_retry":
			m.setNotice(fmt.Sprintf("provider retry %d/%d: %s", a.Attempt, a.Max, transcript.OneLine(a.Message)), false)
		case "user_message_injected":
			if id, err := strconv.ParseUint(a.ID, 10, 64); err == nil {
				if i := m.outIndex(func(o *outItem) bool { return o.acked && o.inputID == id }); i >= 0 {
					m.dropOut(i)
				} else if m.pending > 0 {
					m.early[id] = 0
				}
			}
			m.setNotice("", true)
		}
		t.apply(a)
		m.liveDirty = true
		if a.Type == "turn_finished" {
			m.failed = t.failed
			if a.Status != nil && a.Status.Status == "interrupted" {
				m.setNotice("interrupted", true)
			}
			if t.failed {
				// The error itself stays in the transcript: the live turn
				// shows it, then the saved turn_failed notice.
				m.setNotice("turn failed (enter retries)", false)
			}
			if a.FinalText != "" {
				return tea.Batch(next, m.loadFollowups(a.FinalText))
			}
		}
	case engine.EventTurnEnded:
		m.turnFor(ev.TurnID).ended = true
		if m.turnID == ev.TurnID {
			m.turnID = 0
		}
		var after *uint64
		if m.thread != nil {
			c := m.thread.Cursor
			after = &c
		}
		return tea.Batch(next, m.loadThread(after), m.loadStatus())
	case engine.EventInputsDropped:
		for _, id := range ev.InputIDs {
			if i := m.outIndex(func(o *outItem) bool { return o.acked && o.inputID == id }); i >= 0 {
				m.dropOut(i)
			} else if m.pending > 0 {
				m.early[id] = 0
			}
		}
		m.setNotice("the service dropped "+transcript.Count(len(ev.InputIDs), "message", "messages"), false)
		if m.quiet() && m.thread != nil {
			// A load deferred for the dropped message can run now.
			c := m.thread.Cursor
			return tea.Batch(next, m.loadThread(&c))
		}
	}
	return next
}

func (m *Model) loadFollowups(finalText string) tea.Cmd {
	ctx, be, gen, sent := m.threadCtx, m.be, m.gen, m.nextLocal
	return func() tea.Msg {
		items, err := be.Followups(ctx, finalText)
		if err != nil {
			return nil
		}
		return followupsMsg{gen: gen, sent: sent, items: items}
	}
}

func (m *Model) autoTitle(text string) tea.Cmd {
	ctx, be, id, gen := m.threadCtx, m.be, m.threadID, m.gen
	return func() tea.Msg {
		t, err := be.GenerateTitle(ctx, id, text)
		if err != nil || strings.TrimSpace(t) == "" {
			return nil
		}
		t = strings.TrimSpace(t)
		if _, err := be.UpdateThread(ctx, engine.Update{ThreadID: id, Title: ptrPtr(t)}); err != nil {
			return noticeMsg{text: "title: " + errText(err)}
		}
		return titledMsg{gen: gen, title: t}
	}
}

func ptrPtr(s string) **string { p := &s; return &p }

// busy reports work in flight on the thread: a running turn, a message the
// service has not answered, or one it queued.
func (m *Model) busy() bool { return m.turnID != 0 || m.pending > 0 || len(m.outbox) > 0 }

// send submits the composer: a steer while a turn runs, unless queue.
func (m *Model) send(queue bool) tea.Cmd {
	text := strings.TrimSpace(m.composer.Value())
	if strings.HasPrefix(text, "/") && len(m.attachments) == 0 {
		m.composer.Reset()
		if cmd, ok := m.runCommand(text); ok {
			return cmd
		}
		m.composer.SetValue(text)
		m.setNotice("unknown command "+strings.Fields(text)[0]+" (ctrl+o lists them)", false)
		return nil
	}
	if text == "" && len(m.attachments) == 0 {
		if m.failed && !m.busy() && m.saved {
			return m.rerun()
		}
		return nil
	}
	if m.sub == nil {
		m.setNotice("not connected to the engine service yet", false)
		return nil
	}
	images := make([]engine.Image, 0, len(m.attachments))
	for _, a := range m.attachments {
		images = append(images, a.image)
	}
	steer := m.turnID != 0 && !queue && len(images) == 0
	m.nextLocal++
	m.outbox = append(m.outbox, outItem{local: m.nextLocal, text: text, images: len(images), steer: steer})
	m.liveDirty = true
	m.history = append(m.history, text)
	m.histPos, m.draft = -1, ""
	m.composer.Reset()
	m.attachments = nil
	m.scroll = 0
	m.failed = false
	m.followups = nil
	m.setNotice("", true)
	return m.submit(engine.SubmitRequest{ThreadID: m.threadID, Text: text, Images: images, Steer: steer}, m.nextLocal)
}

// rerun runs the saved history again after a failed turn.
func (m *Model) rerun() tea.Cmd {
	if m.sub == nil {
		m.setNotice("not connected to the engine service yet", false)
		return nil
	}
	m.failed = false
	m.nextLocal++
	m.outbox = append(m.outbox, outItem{local: m.nextLocal})
	return m.submit(engine.SubmitRequest{ThreadID: m.threadID, Rerun: true}, m.nextLocal)
}

func (m *Model) submit(req engine.SubmitRequest, local uint64) tea.Cmd {
	m.pending++
	ctx, be, gen := m.threadCtx, m.be, m.gen
	sel, saved, root := m.pendingSel, m.saved, m.opts.Root
	if !saved {
		m.pendingSel = nil
	}
	return func() tea.Msg {
		if !saved && sel != nil {
			// A model picked before the first message is stored with the
			// thread's root, so the first turn already runs it.
			if _, err := be.UpdateThread(ctx, engine.Update{ThreadID: req.ThreadID, Root: root, Model: ptrPtr(*sel)}); err != nil {
				return submittedMsg{gen: gen, local: local, err: err}
			}
		}
		sub, err := be.Submit(ctx, req)
		return submittedMsg{gen: gen, local: local, sub: sub, err: err}
	}
}

func (m *Model) interrupt() tea.Cmd {
	ctx, be, id := m.threadCtx, m.be, m.threadID
	m.setNotice("interrupting…", true)
	return func() tea.Msg {
		if err := be.Cancel(ctx, id, false); err != nil {
			return noticeMsg{text: "interrupt: " + errText(err)}
		}
		return nil
	}
}

// switchThread shows another thread (or a new one when id is empty). The
// old thread's connections close; its turns keep running in the service.
func (m *Model) switchThread(id string) tea.Cmd {
	m.threadCancel()
	m.threadCtx, m.threadCancel = context.WithCancel(m.ctx)
	m.gen++
	m.sub, m.reconnecting = nil, false
	m.turns, m.turnID, m.outbox, m.pending = nil, 0, nil, 0
	m.early = map[uint64]uint64{}
	m.thread, m.title, m.liveDirty, m.linesKey = nil, "", true, ""
	m.loadSeq, m.appliedSeq = 0, 0
	m.failed, m.firstSent, m.pendingSel, m.hasStat = false, false, nil, false
	m.history, m.histPos, m.scroll = nil, -1, 0
	m.attachments = nil
	if id == "" {
		m.threadID, m.saved = newThreadID(), false
		m.setNotice("new thread", true)
	} else {
		m.threadID, m.saved = id, true
		m.setNotice("", true)
	}
	return tea.Batch(m.subscribe(), m.loadThread(nil), m.loadStatus())
}

func (m *Model) attachImage(path string) tea.Cmd {
	ctx, be := m.ctx, m.be
	name := transcript.OneLine(filepath.Base(path))
	return func() tea.Msg {
		img, err := be.PrepareImage(ctx, path)
		return imageMsg{name: name, image: img, err: err}
	}
}

// imagePath reports a pasted text that is one image file path (plain,
// quoted, or with escaped spaces).
func imagePath(s string) (string, bool) {
	s = strings.TrimSpace(s)
	if strings.ContainsAny(s, "\n") || s == "" {
		return "", false
	}
	if len(s) >= 2 && (s[0] == '\'' || s[0] == '"') && s[len(s)-1] == s[0] {
		s = s[1 : len(s)-1]
	}
	s = strings.ReplaceAll(s, `\ `, " ")
	s = strings.TrimPrefix(s, "file://")
	switch strings.ToLower(filepath.Ext(s)) {
	case ".png", ".jpg", ".jpeg", ".gif", ".webp":
	default:
		return "", false
	}
	if info, err := os.Stat(s); err != nil || info.IsDir() {
		return "", false
	}
	return s, true
}

func (m *Model) toggleVoice() tea.Cmd {
	if m.transcribe {
		return nil
	}
	if m.rec == nil {
		rec, err := startRecording(m.ctx)
		if err != nil {
			m.setNotice(err.Error(), false)
			return nil
		}
		m.rec = rec
		m.setNotice("recording… ctrl+space to stop, esc to discard", true)
		return nil
	}
	rec := m.rec
	m.rec = nil
	m.transcribe = true
	m.setNotice("transcribing…", true)
	ctx, be := m.ctx, m.be
	return func() tea.Msg {
		defer rec.cleanup()
		path, err := rec.stop()
		if err != nil {
			return transcribedMsg{err: err}
		}
		text, err := be.TranscribeAudio(ctx, path, "audio/wav")
		return transcribedMsg{text: text, err: err}
	}
}

func (m *Model) discardRecording() {
	m.rec.discard()
	m.rec = nil
	m.setNotice("recording discarded", true)
}

func (m *Model) handleKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	k := msg.String()
	if m.overlay != nil {
		return m.overlayKey(msg), true
	}
	if m.detail != nil {
		return m.detailKey(msg), true
	}
	switch k {
	case "ctrl+c":
		switch {
		case m.rec != nil:
			m.discardRecording()
		case m.turnID != 0:
			return m.interrupt(), true
		case m.composer.Value() != "" || len(m.attachments) > 0:
			m.composer.Reset()
			m.attachments = nil
		default:
			return m.quit(), true
		}
		return nil, true
	case "ctrl+d":
		if m.composer.Value() == "" {
			return m.quit(), true
		}
		return nil, false
	case "esc":
		switch {
		case m.rec != nil:
			m.discardRecording()
		case m.turnID != 0:
			return m.interrupt(), true
		case m.scroll > 0:
			m.scroll = 0
		}
		return nil, true
	case "enter":
		return m.send(false), true
	case "tab":
		if m.turnID != 0 {
			return m.send(true), true
		}
		return nil, true
	case "pgup":
		m.scrollBy(max(m.bodyRows()-2, 1))
		return nil, true
	case "pgdown":
		m.scrollBy(-max(m.bodyRows()-2, 1))
		return nil, true
	case "ctrl+home":
		m.scroll = 1 << 30
		return nil, true
	case "ctrl+end":
		m.scroll = 0
		return nil, true
	case "ctrl+l":
		return m.openModelPicker(), true
	case "ctrl+t":
		return m.openThinkingPicker(), true
	case "ctrl+p":
		return m.openThreadPicker(), true
	case "ctrl+n":
		return m.newThread(), true
	case "ctrl+o":
		m.openPalette()
		return nil, true
	case "ctrl+f":
		m.openFollowups()
		return nil, true
	case "ctrl+space", "ctrl+@":
		return m.toggleVoice(), true
	case "alt+t":
		m.toggleThinking()
		return nil, true
	case "up":
		if m.composer.Line() == 0 && m.historyStep(-1) {
			return nil, true
		}
	case "down":
		if m.composer.Line() == m.composer.LineCount()-1 && m.historyStep(1) {
			return nil, true
		}
	}
	return nil, false
}

func (m *Model) toggleThinking() {
	m.thinking = !m.thinking
	m.linesKey, m.liveDirty = "", true
}

func (m *Model) newThread() tea.Cmd {
	if m.busy() {
		m.setNotice("this thread has work in flight; wait or interrupt it first (esc)", false)
		return nil
	}
	return m.switchThread("")
}

// historyStep walks the sent messages; the draft comes back at the end.
func (m *Model) historyStep(dir int) bool {
	if len(m.history) == 0 {
		return false
	}
	switch {
	case m.histPos < 0 && dir < 0:
		m.draft = m.composer.Value()
		m.histPos = len(m.history) - 1
	case m.histPos < 0:
		return false
	default:
		m.histPos += dir
	}
	if m.histPos >= len(m.history) {
		m.histPos = -1
		m.composer.SetValue(m.draft)
		return true
	}
	m.histPos = max(m.histPos, 0)
	m.composer.SetValue(m.history[m.histPos])
	return true
}

func (m *Model) scrollBy(n int) {
	m.scroll = max(m.scroll+n, 0)
}
