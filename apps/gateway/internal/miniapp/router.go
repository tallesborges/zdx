package miniapp

import (
	"net/http"
	"strings"
	"time"
	"unicode/utf8"
)

// call is one authorized API request: the verified Telegram user and the
// decoded `{id}` path parameter (empty on routes without one).
type call struct {
	user int64
	id   string
	// badID is set when the raw `{id}` did not decode to UTF-8. Handlers
	// answer it at the point the Rust handler extracts its path.
	badID bool
}

// badIDResponse is axum's answer to a path parameter that is not UTF-8.
func badIDResponse(w http.ResponseWriter) {
	writeText(w, http.StatusBadRequest, "Invalid URL: Invalid UTF-8 in `id`")
}

type apiHandler func(http.ResponseWriter, *http.Request, call)

// apiRoute is one `/api` route. Segments are matched against the raw
// (still percent-encoded) path, as axum's router does; `{id}` is the only
// parameter.
type apiRoute struct {
	method   string
	segments []string
	handler  apiHandler
}

func (s *server) apiRoutes() []apiRoute {
	get := func(path string, h apiHandler) apiRoute {
		return apiRoute{http.MethodGet, strings.Split(path, "/"), h}
	}
	return []apiRoute{
		get("threads", s.getThreads),
		get("threads/{id}", s.getThread),
		get("threads/{id}/trajectory", s.getTrajectory),
		get("threads/{id}/workers", s.getWorkers),
		get("threads/{id}/artifacts", s.getArtifacts),
		get("threads/{id}/artifacts/file", s.getArtifactFile),
		get("threads/{id}/artifacts/version", s.getArtifactVersion),
		get("threads/{id}/artifacts/image", s.getArtifactImage),
		{http.MethodPost, strings.Split("threads/{id}/artifacts/feedback", "/"), s.postFeedback},
		get("monitor", s.getMonitor),
		get("workspaces", s.getWorkspaces),
		get("usage", s.getUsage),
		get("git", s.getGit),
		get("git/scope", s.getGitScope),
		get("git/diff", s.getGitDiff),
	}
}

// match reports whether raw path segments fit the route, and the raw `{id}`.
// Like axum (matchit), a parameter matches an empty segment only when more
// segments follow: `/api/threads//workers` matches, `/api/threads/` does not.
func (route apiRoute) match(segments []string) (string, bool) {
	if len(segments) != len(route.segments) {
		return "", false
	}
	id := ""
	for i, want := range route.segments {
		if want == "{id}" {
			if segments[i] == "" && i == len(segments)-1 {
				return "", false
			}
			id = segments[i]
			continue
		}
		if segments[i] != want {
			return "", false
		}
	}
	return id, true
}

// routes returns the whole Mini App handler. Routing is done here rather
// than by http.ServeMux, which cleans paths with redirects, answers a wrong
// method before authorization, and treats empty segments differently from
// the Rust router this replaces.
func (s *server) routes() http.Handler {
	api := s.apiRoutes()
	return gzipMiddleware(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw := r.URL.EscapedPath()
		switch {
		case raw == "/app" || raw == "/threads" || raw == "/monitor":
			if !methodAllowed(w, r, http.MethodGet) {
				return
			}
			s.serveApp(w, r)
		case strings.HasPrefix(raw, "/app/") && len(raw) > len("/app/"):
			if !methodAllowed(w, r, http.MethodGet) {
				return
			}
			path, ok := decodeParam(raw[len("/app/"):])
			if !ok {
				writeText(w, http.StatusBadRequest, "Invalid URL: Invalid UTF-8 in `path`")
				return
			}
			s.serveAsset(w, path)
		case strings.HasPrefix(raw, "/api/"):
			s.serveAPI(w, r, api, strings.Split(raw[len("/api/"):], "/"))
		default:
			writeEmpty(w, http.StatusNotFound)
		}
	}))
}

func (s *server) serveAPI(w http.ResponseWriter, r *http.Request, routes []apiRoute, segments []string) {
	var matched *apiRoute
	rawID := ""
	allowed := ""
	for i := range routes {
		id, ok := routes[i].match(segments)
		if !ok {
			continue
		}
		allowed = allowHeader(routes[i].method)
		if methodMatches(r.Method, routes[i].method) {
			matched, rawID = &routes[i], id
			break
		}
	}
	if allowed == "" {
		writeEmpty(w, http.StatusNotFound)
		return
	}
	started := time.Now()
	recorder := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
	defer func() { s.logAPI(r, recorder.status, time.Since(started)) }()

	// Authorization runs before anything else on a known path, even a wrong
	// method (the Rust router's route layer does the same).
	user, failure := s.authorize(r)
	if failure != nil {
		if matched == nil {
			recorder.Header().Set("Allow", allowed)
		}
		writeText(recorder, failure.status, failure.message)
		return
	}
	if matched == nil {
		recorder.Header().Set("Allow", allowed)
		writeEmpty(recorder, http.StatusMethodNotAllowed)
		return
	}
	c := call{user: user}
	c.id, c.badID = rawID, false
	if id, ok := decodeParam(rawID); ok {
		c.id = id
	} else {
		c.id, c.badID = "", true
	}
	// The feedback route takes its upload slot before it reads the path;
	// every other route reads the path first.
	if c.badID && matched.method != http.MethodPost {
		badIDResponse(recorder)
		return
	}
	matched.handler(recorder, r, c)
}

func methodMatches(got, want string) bool {
	return got == want || (want == http.MethodGet && got == http.MethodHead)
}

func allowHeader(method string) string {
	if method == http.MethodGet {
		return "GET,HEAD"
	}
	return method
}

func methodAllowed(w http.ResponseWriter, r *http.Request, method string) bool {
	if methodMatches(r.Method, method) {
		return true
	}
	w.Header().Set("Allow", allowHeader(method))
	writeEmpty(w, http.StatusMethodNotAllowed)
	return false
}

// decodeParam percent-decodes a raw path parameter the way axum does:
// invalid escapes stay literal, `+` stays `+`, and the result must be UTF-8.
func decodeParam(raw string) (string, bool) {
	out := make([]byte, 0, len(raw))
	for i := 0; i < len(raw); i++ {
		c := raw[i]
		if c == '%' && i+2 < len(raw) && isHex(raw[i+1]) && isHex(raw[i+2]) {
			out = append(out, unhex(raw[i+1])<<4|unhex(raw[i+2]))
			i += 2
			continue
		}
		out = append(out, c)
	}
	if !utf8.Valid(out) {
		return "", false
	}
	return string(out), true
}

func writeEmpty(w http.ResponseWriter, status int) {
	w.Header().Set("Content-Length", "0")
	w.WriteHeader(status)
}
