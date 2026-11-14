package bot

import (
	"context"
	"fmt"
	"os"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
)

// ServiceResolver resolves topics with the Rust resolve_thread op.
type ServiceResolver struct{ Engine *engine.Client }

var _ TopicResolver = ServiceResolver{}

// ResolveTopic implements TopicResolver.
func (r ServiceResolver) ResolveTopic(ctx context.Context, chat int64, topic *int64) (TopicInfo, error) {
	route, err := r.Engine.ResolveThread(ctx, chat, topic)
	if err != nil {
		return TopicInfo{}, err
	}
	info := TopicInfo{ThreadID: route.EffectiveThreadID, PendingTopicTitle: route.PendingTopicTitle, Exists: route.Exists}
	if route.Profile != nil {
		info.Profile = *route.Profile
	}
	switch route.Kind {
	case engine.KindOrdinary:
		info.Kind = TopicOrdinary
	case engine.KindProfile:
		info.Kind = TopicProfile
	case engine.KindWorkerMirror:
		info.Kind = TopicWorkerMirror
		info.WorkerThreadID = route.EffectiveThreadID
	default:
		return TopicInfo{}, fmt.Errorf("the orchestrator service reported an unknown thread kind %q", route.Kind)
	}
	return info, nil
}

// ServicePlanner plans replies from Rust reply parts: Rust strips the
// follow-ups and media blocks and classifies each file; render turns that
// into Telegram messages.
type ServicePlanner struct {
	Engine      *engine.Client
	MiniAppBase string
}

var _ ReplyPlanner = ServicePlanner{}

// PlanReply implements ReplyPlanner.
func (p ServicePlanner) PlanReply(ctx context.Context, finalText string, target ReplyTarget, threadID string, workers []string) (render.Plan, error) {
	parts, err := p.Engine.ReplyParts(ctx, finalText)
	if err != nil {
		return render.Plan{}, err
	}
	return PlanFromParts(parts, p.MiniAppBase, target, threadID, p.workerLinks(ctx, threadID, workers)), nil
}

// workerLinks resolves touched workers as the Rust bot does: the legacy
// mirror topic (the worker's mirror_url) or the Mini App, and the title
// (thread_workers of the turn's thread; the first 8 characters of the id
// when it is not there). A worker with neither link is left out; a failed
// lookup only drops the mirror link or the title.
func (p ServicePlanner) workerLinks(ctx context.Context, threadID string, workers []string) []render.WorkerLink {
	if len(workers) == 0 {
		return nil
	}
	titles := WorkerTitles(ctx, p.Engine, threadID)
	var links []render.WorkerLink
	for _, id := range workers {
		mirror := ""
		if w, err := p.Engine.GetWorker(ctx, id); err == nil && w != nil && w.MirrorURL != nil {
			mirror = *w.MirrorURL
		}
		url, ok := render.WorkerURL(mirror, p.MiniAppBase, id)
		if !ok {
			continue
		}
		links = append(links, render.WorkerLink{Title: titles[id], ID: id, URL: url})
	}
	return links
}

// PlanFromParts builds the render plan of reply parts. Every media entry
// stays in the plan, in Rust's order; files that are not sendable fail
// without a send, so the record and its summary name them. The send method
// is Rust's send_as. A card shows the file's current size.
func PlanFromParts(parts engine.ReplyParts, miniAppBase string, target ReplyTarget, threadID string, workers []render.WorkerLink) render.Plan {
	reply := render.Reply{HTML: render.ToTelegramHTML(parts.Text), Followups: parts.Followups}
	for _, m := range parts.Media {
		f := render.File{
			Path: m.Path, Missing: !m.Sendable, ArtifactKey: m.ArtifactKey,
			Linkable: m.LexicalIsPhysical, HTML: m.Kind == "html",
		}
		// An unknown send_as leaves the method unset: the item fails
		// without a send instead of guessing one.
		if method, err := render.MethodFromSendAs(m.SendAs); err == nil {
			f.Method = method
		}
		if info, err := os.Stat(m.Path); err == nil {
			f.Size = info.Size()
		}
		reply.Files = append(reply.Files, f)
	}
	return render.PlanReply(reply, render.Options{
		MiniAppBase:   miniAppBase,
		ThreadID:      threadID,
		TopicThreadID: conversation.ThreadID(target.Chat, target.Topic),
		Workers:       workers,
	})
}
