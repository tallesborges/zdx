package miniapp

import (
	"compress/gzip"
	"io/fs"
	"mime"
	"net/http"
	"path"
	"strconv"
	"strings"
)

type assetSource struct{ fs fs.FS }

// read returns a regular file of the built app. Paths that are not clean
// relative paths (`..`, leading `/`, empty) and directories are absent, as
// they are from rust-embed's file map.
func (a assetSource) read(name string) ([]byte, bool) {
	if a.fs == nil || !fs.ValidPath(name) || name == "." {
		return nil, false
	}
	info, err := fs.Stat(a.fs, name)
	if err != nil || !info.Mode().IsRegular() {
		return nil, false
	}
	data, err := fs.ReadFile(a.fs, name)
	if err != nil {
		return nil, false
	}
	return data, true
}

// serveApp serves the SPA entry point, never cached, so a new build's
// content-hashed asset names are picked up on the next open.
func (s *server) serveApp(w http.ResponseWriter, _ *http.Request) {
	data, ok := s.assets.read("index.html")
	if !ok {
		writeText(w, http.StatusInternalServerError, "Mini App assets are missing; run `just web-build`")
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// serveAsset serves a content-hashed asset, cached for a year: Vite renames
// them on every build, and the iOS WebView has no service workers.
func (s *server) serveAsset(w http.ResponseWriter, name string) {
	data, ok := s.assets.read(name)
	if !ok {
		writeText(w, http.StatusNotFound, "Not found")
		return
	}
	contentType := mime.TypeByExtension(path.Ext(name))
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	w.Header().Set("Content-Type", contentType)
	w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// gzipMinBytes is tower-http's default size floor.
const gzipMinBytes = 32

// gzipMiddleware is tower-http's CompressionLayer with only gzip enabled
// and its default predicate: a response is compressible unless it has
// Content-Encoding or Content-Range, a known size under 32 bytes, or a
// Content-Type starting with `image/` (except `image/svg+xml`),
// `text/event-stream` or `application/grpc` (case-sensitive prefixes, as
// there). A compressible response gets `Vary: accept-encoding` whatever the
// client accepts, and is gzipped when the client prefers gzip.
func gzipMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gw := &gzipWriter{ResponseWriter: w, gzip: prefersGzip(r.Header.Values("Accept-Encoding"))}
		defer gw.close()
		next.ServeHTTP(gw, r)
	})
}

// prefersGzip picks the encoding as tower-http does: entries of
// `Accept-Encoding` it knows (gzip, x-gzip, identity), each with an
// optional `q=` of at most three decimals in 0..1 (an entry with a bad
// q-value is ignored); the highest q above 0 wins, gzip on a tie.
func prefersGzip(values []string) bool {
	best, bestGzip := -1, false
	for _, value := range values {
		if _, ok := visibleHeader([]string{value}); !ok {
			continue
		}
		for _, item := range strings.Split(value, ",") {
			name, param, hasParam := strings.Cut(item, ";")
			name = strings.TrimSpace(name)
			isGzip := strings.EqualFold(name, "gzip") || strings.EqualFold(name, "x-gzip")
			if !isGzip && !strings.EqualFold(name, "identity") {
				continue
			}
			q := 1000
			if hasParam {
				parsed, ok := parseQValue(strings.TrimSpace(param))
				if !ok {
					continue
				}
				q = parsed
			}
			if q == 0 {
				continue
			}
			if q > best || (q == best && isGzip) {
				best, bestGzip = q, isGzip
			}
		}
	}
	return bestGzip
}

// parseQValue is tower-http's `QValue::parse`, in thousandths.
func parseQValue(s string) (int, bool) {
	if len(s) < 3 || (s[0] != 'q' && s[0] != 'Q') || s[1] != '=' {
		return 0, false
	}
	var value int
	switch s[2] {
	case '0':
		value = 0
	case '1':
		value = 1000
	default:
		return 0, false
	}
	rest := s[3:]
	if rest == "" {
		return value, true
	}
	if rest[0] != '.' {
		return 0, false
	}
	factor := 100
	for _, c := range rest[1:] {
		if c < '0' || c > '9' || factor < 1 {
			return 0, false
		}
		value += factor * int(c-'0')
		factor /= 10
	}
	return value, value <= 1000
}

func compressible(h http.Header) bool {
	if h.Get("Content-Encoding") != "" || h.Get("Content-Range") != "" {
		return false
	}
	if length := h.Get("Content-Length"); length != "" {
		if n, err := strconv.ParseUint(length, 10, 64); err == nil && n < gzipMinBytes {
			return false
		}
	}
	contentType := h.Get("Content-Type")
	switch {
	case strings.HasPrefix(contentType, "image/") && !strings.HasPrefix(contentType, "image/svg+xml"):
		return false
	case strings.HasPrefix(contentType, "text/event-stream"), strings.HasPrefix(contentType, "application/grpc"):
		return false
	}
	return true
}

type gzipWriter struct {
	http.ResponseWriter
	gzip        bool
	gz          *gzip.Writer
	wroteHeader bool
}

func (g *gzipWriter) WriteHeader(status int) {
	if g.wroteHeader {
		return
	}
	g.wroteHeader = true
	h := g.ResponseWriter.Header()
	// No body can follow these, so there is nothing to encode.
	bodiless := status < 200 || status == http.StatusNoContent || status == http.StatusNotModified
	if !bodiless && compressible(h) {
		if !varyHasAcceptEncoding(h) {
			h.Add("Vary", "accept-encoding")
		}
		if g.gzip {
			h.Del("Accept-Ranges")
			h.Del("Content-Length")
			h.Set("Content-Encoding", "gzip")
			g.gz = gzip.NewWriter(g.ResponseWriter)
		}
	}
	g.ResponseWriter.WriteHeader(status)
}

func varyHasAcceptEncoding(h http.Header) bool {
	for _, value := range h.Values("Vary") {
		if strings.Contains(strings.ToLower(value), "accept-encoding") {
			return true
		}
	}
	return false
}

func (g *gzipWriter) Write(p []byte) (int, error) {
	if !g.wroteHeader {
		g.WriteHeader(http.StatusOK)
	}
	if g.gz != nil {
		return g.gz.Write(p)
	}
	return g.ResponseWriter.Write(p)
}

func (g *gzipWriter) close() {
	if !g.wroteHeader {
		return
	}
	if g.gz != nil {
		_ = g.gz.Close()
	}
}

func (g *gzipWriter) Unwrap() http.ResponseWriter { return g.ResponseWriter }
