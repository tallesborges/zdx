package ui

import (
	"context"
	"errors"
	"sort"
	"time"

	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
	"github.com/tallesborges/zdx/apps/shared/transcript"
)

// Idle parents: a context row of the agents tree is a thread with active
// children and no run of its own. Its title and the model of its last
// request come from `thread_view`: the first look reads the whole thread,
// later looks ask only for what is past the cursor. The model is what the
// last request used (its `usage` item); the model a new turn would get is
// not in any view (catalog `thread_status` resolves it, but only against
// the chat's root, which the monitor does not know), so it is never shown
// as the current model.

// maxParentLookups bounds the threads one fetch reads. With more idle
// parents, each fetch reads the ones answered longest ago, so every parent
// gets its turn.
const maxParentLookups = 8

// maxKeptParents bounds the parents kept while the tree does not show them
// (the parent runs, or its children ended). A parent that comes back
// continues from its cursor instead of reading its whole thread again.
const maxKeptParents = 32

// parentInfo is what the monitor knows about one idle parent.
type parentInfo struct {
	title    string
	sourceID string
	cursor   *uint64 // nil: the next look reads the whole thread
	// model and provider of the last request; empty when the thread has
	// made none (or none in what was read).
	model, provider string
	usedAt          string
	loaded          bool  // a look succeeded at least once
	missing         bool  // the service says the thread does not exist
	err             error // the last look failed (data, if any, is older)

	shown  bool   // an idle parent of the tree at the last sync
	seen   uint64 // the last sync that showed it
	looked uint64 // order of its last answer; 0: never answered
}

// parentLookup is one thread to look at and where the last look ended.
type parentLookup struct {
	id       string
	cursor   *uint64
	sourceID string
}

type parentsResult map[string]parentInfo

// idleParents are the idle parents of the tree, in tree order.
func (t *agentsTab) idleParents() []string {
	var ids []string
	for _, n := range t.tree {
		if n.run < 0 {
			ids = append(ids, n.thread)
		}
	}
	return ids
}

// syncParents marks the idle parents the tree shows, keeps a bounded
// number of the others, and reports whether a parent needs a look now: one
// never answered, or one the tree shows again.
func (t *agentsTab) syncParents() bool {
	if t.parents == nil {
		t.parents = map[string]parentInfo{}
	}
	t.parentSync++
	due := false
	for _, id := range t.idleParents() {
		p, ok := t.parents[id]
		if !ok || !p.shown || p.looked == 0 {
			due = true
		}
		p.shown, p.seen = true, t.parentSync
		t.parents[id] = p
	}
	var hidden []string
	for id, p := range t.parents {
		if p.seen != t.parentSync {
			p.shown = false
			t.parents[id] = p
			hidden = append(hidden, id)
		}
	}
	if len(hidden) > maxKeptParents {
		sort.Slice(hidden, func(i, j int) bool {
			a, b := t.parents[hidden[i]].seen, t.parents[hidden[j]].seen
			if a != b {
				return a < b
			}
			return hidden[i] < hidden[j]
		})
		for _, id := range hidden[:len(hidden)-maxKeptParents] {
			delete(t.parents, id)
		}
	}
	return due
}

// parentLookups are the next parents to read: never answered first, then
// the ones answered longest ago, at most maxParentLookups.
func (t *agentsTab) parentLookups() []parentLookup {
	ids := t.idleParents()
	sort.SliceStable(ids, func(i, j int) bool { return t.parents[ids[i]].looked < t.parents[ids[j]].looked })
	var out []parentLookup
	for _, id := range ids[:min(len(ids), maxParentLookups)] {
		p := t.parents[id]
		out = append(out, parentLookup{id: id, cursor: p.cursor, sourceID: p.sourceID})
	}
	return out
}

// lookParents reads each parent: in full without a cursor, else the delta.
func lookParents(ctx context.Context, b Backend, lookups []parentLookup) parentsResult {
	out := parentsResult{}
	for _, l := range lookups {
		if ctx.Err() != nil {
			break
		}
		th, _, err := b.Thread(ctx, l.id, l.cursor)
		out[l.id] = parentFrom(l, th, err)
	}
	return out
}

// parentFrom turns one answer into what the row shows. A failed look keeps
// nothing of its own; merge keeps the older data.
func parentFrom(l parentLookup, th data.Thread, err error) parentInfo {
	if err != nil {
		var se *wire.ServiceError
		if errors.As(err, &se) && se.Code == "not_found" {
			return parentInfo{missing: true, loaded: true}
		}
		return parentInfo{err: err}
	}
	p := parentInfo{loaded: true, title: th.Title, sourceID: th.SourceID}
	cursor := th.Cursor
	p.cursor = &cursor
	if th.Partial && l.cursor != nil && (th.Cursor < *l.cursor || th.SourceID != l.sourceID) {
		// Reset history or a moved alias: read it all next time.
		p.cursor = nil
		return p
	}
	for i := len(th.Activity) - 1; i >= 0; i-- {
		if a := th.Activity[i]; a.Type == "usage" && str(a.Model) != "" {
			p.model, p.provider, p.usedAt = str(a.Model), str(a.Provider), a.Time
			break
		}
	}
	return p
}

// mergeParent folds a look into what was known: a delta without a request
// keeps the older model; a failure keeps the older data and records the
// error.
func mergeParent(old, next parentInfo, full bool) parentInfo {
	switch {
	case next.err != nil:
		old.err = next.err
		return old
	case next.missing:
		return next
	case next.cursor == nil:
		// Reset: drop the model; the next look reads it all.
		next.model, next.provider, next.usedAt = "", "", ""
		return next
	case !full && next.model == "":
		next.model, next.provider, next.usedAt = old.model, old.provider, old.usedAt
	}
	return next
}

// applyParents folds one fetch into the cache. An answer is dropped when
// its entry changed since the look was sent (dropped and added again, or
// reset): merging a delta into an entry that never had the full read
// would hide the model.
func (m *Model) applyParents(r parentsResult, lookups []parentLookup) {
	t := &m.agents
	for _, l := range lookups {
		next, answered := r[l.id]
		if !answered {
			// The budget ended first; it keeps its turn.
			continue
		}
		old, kept := t.parents[l.id]
		if !kept || !sameCursor(old.cursor, l.cursor) || old.sourceID != l.sourceID {
			continue
		}
		next.title, next.model, next.provider = transcript.Clean(next.title), transcript.Clean(next.model), transcript.Clean(next.provider)
		next.sourceID, next.usedAt = transcript.Clean(next.sourceID), transcript.Clean(next.usedAt)
		t.parentLooks++
		merged := mergeParent(old, next, l.cursor == nil)
		merged.shown, merged.seen, merged.looked = old.shown, old.seen, t.parentLooks
		t.parents[l.id] = merged
	}
}

func sameCursor(a, b *uint64) bool {
	return (a == nil && b == nil) || (a != nil && b != nil && *a == *b)
}

// parentAgent is the last request's model and provider in the shape the
// spec helpers take; no thinking level is recorded for a request.
func parentAgent(p parentInfo) data.Agent {
	a := data.Agent{}
	if p.model != "" {
		a.Model = &p.model
	}
	if p.provider != "" {
		a.Provider = &p.provider
	}
	return a
}

// parentSpec is the last-used model as `provider:model`, built by the same
// rules as a run's spec.
func parentSpec(p parentInfo) string { return modelSpec(parentAgent(p)) }

// parentRow is the RUN and MODEL cells of an idle parent.
func parentRow(p parentInfo, ok bool, titleMax int) (run, model cell) {
	switch {
	case !ok || (!p.loaded && p.err == nil):
		return cs("idle parent", dim), cs("…", dim)
	case p.missing:
		return cs("idle parent (not found)", dim), cs("—", dim)
	}
	name := p.title
	if name == "" {
		name = "idle parent"
	}
	// The full title is in the detail pane; a long one must not widen the
	// RUN column and narrow MODEL for every row.
	run = cs(ansi.Truncate(name, titleMax, "…"), dim)
	if spec := parentSpec(p); spec != "" {
		model = cs(spec, dim)
		a := parentAgent(p)
		model.shorten = func(w int) string { return shortSpec(a, w) }
	} else {
		model = cs("—", dim)
	}
	return run, model
}

// parentDetail is the selected idle parent's pane.
func parentDetail(n agentNode, p parentInfo, ok bool, _ time.Time) []string {
	lines := []string{}
	if ok && p.title != "" {
		lines = append(lines, kv("Title", bold.Render(p.title)))
	}
	lines = append(lines, kv("Thread", n.thread))
	if ok && p.sourceID != "" && p.sourceID != n.thread {
		lines = append(lines, kv("", warn.Render("alias of "+p.sourceID)))
	}
	lines = append(lines, kv("Activity", dim.Render("idle: no run of its own")))
	switch {
	case !ok || (!p.loaded && p.err == nil):
		lines = append(lines, kv("Model", dim.Render("looking up…")))
	case p.missing:
		lines = append(lines, kv("Model", dim.Render("unknown: the service has no thread with this id")))
	case parentSpec(p) != "":
		used := ""
		if p.usedAt != "" {
			used = " at " + p.usedAt
		}
		lines = append(lines, kv("Model", parentSpec(p)+dim.Render(" · last request"+used+"; thinking not recorded")))
	default:
		lines = append(lines, kv("Model", dim.Render("unknown: no request recorded yet")))
	}
	if ok && p.err != nil {
		lines = append(lines, kv("", warn.Render("lookup failed: "+oneLine(p.err.Error()))))
	}
	lines = append(lines, "  "+dim.Render("Parent of "+count(n.kids, "active run", "active runs")+" below; shown for context, not counted as a run."))
	return lines
}

// parentsFetcher reads the wanted parents, or nil when there are none.
func (m *Model) parentsFetcher() fetchFunc {
	lookups := m.agents.parentLookups()
	if len(lookups) == 0 {
		return nil
	}
	b := m.backend
	return func(ctx context.Context) (any, source.Origin, error) {
		// Partial results count: a slow thread must not discard the ones
		// already read. A look the budget cut short records its error.
		r := lookParents(ctx, b, lookups)
		return parentsAnswer{lookups: lookups, result: r}, source.OriginSocket, nil
	}
}

type parentsAnswer struct {
	lookups []parentLookup
	result  parentsResult
}
