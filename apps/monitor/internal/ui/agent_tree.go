package ui

import (
	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

// agentNode is one row of the agents tree: a run, or a parent thread that
// has active children but no run of its own (a context row, not counted
// as active).
type agentNode struct {
	run    int    // index in rows; -1 for a context row
	thread string // the context row's thread id
	depth  int
	prefix string // tree guide, e.g. "│  └─ "
	kids   int    // direct children
}

func (n agentNode) id(rows []data.Agent) string {
	if n.run < 0 {
		return "parent:" + n.thread
	}
	return agentID(rows[n.run])
}

// agentTree nests runs under their parent run, using only the lineage the
// run markers report: `parent_thread_id` (the owner of a worker, the
// caller of a subagent or helper) matched to a running run's `thread_id`.
// A parent with no run becomes a context row; a run without a parent, or
// one whose parent link would close a cycle, is a root. Order follows the
// engine's order (oldest first) among siblings.
func agentTree(rows []data.Agent) []agentNode {
	byThread := map[string]int{}
	for i, a := range rows {
		if id := str(a.ThreadID); id != "" {
			if _, seen := byThread[id]; !seen {
				byThread[id] = i
			}
		}
	}
	// parent[i]: run index, -1 for a root, or -2 for a context parent.
	parent := make([]int, len(rows))
	for i := range rows {
		parent[i] = -1
	}
	accepted := func(from, target int) bool {
		// Would i -> from close a loop through accepted links?
		for seen := 0; from >= 0 && seen <= len(rows); seen++ {
			if from == target {
				return false
			}
			from = parent[from]
		}
		return true
	}
	for i, a := range rows {
		p := str(a.ParentThreadID)
		if p == "" || p == str(a.ThreadID) {
			continue
		}
		if j, ok := byThread[p]; ok {
			if j != i && accepted(j, i) {
				parent[i] = j
			}
			continue
		}
		parent[i] = -2
	}

	children := map[int][]int{}
	contextKids := map[string][]int{}
	type root struct {
		run    int
		thread string
	}
	var roots []root
	seenContext := map[string]bool{}
	for i, a := range rows {
		switch parent[i] {
		case -1:
			roots = append(roots, root{run: i})
		case -2:
			p := str(a.ParentThreadID)
			if !seenContext[p] {
				seenContext[p] = true
				roots = append(roots, root{run: -1, thread: p})
			}
			contextKids[p] = append(contextKids[p], i)
		default:
			children[parent[i]] = append(children[parent[i]], i)
		}
	}

	var out []agentNode
	var walk func(run int, depth int, guide string, last bool)
	walk = func(run, depth int, guide string, last bool) {
		prefix := ""
		next := ""
		if depth > 0 {
			if last {
				prefix, next = guide+"└─ ", guide+"   "
			} else {
				prefix, next = guide+"├─ ", guide+"│  "
			}
		}
		kids := children[run]
		out = append(out, agentNode{run: run, depth: depth, prefix: prefix, kids: len(kids)})
		for k, c := range kids {
			walk(c, depth+1, next, k == len(kids)-1)
		}
	}
	for _, r := range roots {
		if r.run >= 0 {
			walk(r.run, 0, "", true)
			continue
		}
		kids := contextKids[r.thread]
		out = append(out, agentNode{run: -1, thread: r.thread, kids: len(kids)})
		for k, c := range kids {
			walk(c, 1, "", k == len(kids)-1)
		}
	}
	return out
}
