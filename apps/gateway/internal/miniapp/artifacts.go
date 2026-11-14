package miniapp

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// artifactCSP is sent on every response that carries artifact bytes. The
// app reads them with fetch(), which ignores it, and shows HTML only in an
// opaque-origin iframe built from a blob (ArtifactAnnotator.svelte). It
// matters only if a response is ever rendered as a document on the app's
// origin: then the page gets an opaque origin with scripts, forms and
// popups off, so it cannot read the app's storage or call the API. Today
// that needs the Authorization header, which a navigation never sends.
const artifactCSP = "sandbox"

// imageReads is how many canonical image previews are served at once
// (server.rs IMAGE_READS). Requests wait for a slot: a preview is a read.
const imageReads = 2

func (s *server) getArtifacts(w http.ResponseWriter, r *http.Request, c call) {
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return
	}
	body, err := s.engine.ArtifactsList(r.Context(), c.id)
	if err != nil {
		s.viewFailure(w, err, "Artifact list", "Artifact list failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

// artifactParams reads the thread id and the `path` query every artifact
// file route takes, answering bad input itself, in the Rust order: query,
// then thread id, then the path.
func artifactParams(w http.ResponseWriter, r *http.Request, c call) (string, string, bool) {
	query, ok := parseQuery(w, r, queryField{name: "path", required: true})
	if !ok {
		return "", "", false
	}
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return "", "", false
	}
	path, _ := query.text("path")
	if blank(path) || strings.ContainsRune(path, 0) {
		writeText(w, http.StatusBadRequest, "Invalid artifact path")
		return "", "", false
	}
	return c.id, path, true
}

// getArtifactFile serves one listed artifact inline, unchanged. Only the
// engine decides what a thread may serve and under which name; Go opens
// only the absolute regular file it resolved.
func (s *server) getArtifactFile(w http.ResponseWriter, r *http.Request, c call) {
	id, path, ok := artifactParams(w, r, c)
	if !ok {
		return
	}
	resolved, err := s.engine.ArtifactResolve(r.Context(), id, path)
	if err != nil {
		s.viewFailure(w, err, "Artifact files", "Artifact read failed")
		return
	}
	// A malformed resolution is an engine fault, not a missing file: Go
	// never guesses a name or serves a relative path.
	if !filepath.IsAbs(resolved.Path) || resolved.Mime == "" || resolved.Filename == "" ||
		strings.ContainsRune(resolved.Filename, '"') || !headerValue(resolved.Filename) || !headerValue(resolved.Mime) {
		s.log.Error("Engine resolved an artifact without a usable path, type or name", "thread_id", id)
		writeText(w, http.StatusInternalServerError, "Artifact read failed")
		return
	}
	file, info, err := openRegular(resolved.Path)
	if err != nil {
		writeText(w, http.StatusNotFound, "Artifact not found")
		return
	}
	defer file.Close()
	h := w.Header()
	h.Set("Content-Type", resolved.Mime)
	h.Set("Content-Disposition", fmt.Sprintf(`inline; filename="%s"`, resolved.Filename))
	h.Set("Cache-Control", "private, no-store")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Content-Security-Policy", artifactCSP)
	h.Set("Content-Length", strconv.FormatInt(info.Size(), 10))
	w.WriteHeader(http.StatusOK)
	if _, err := io.Copy(w, io.LimitReader(file, info.Size())); err != nil {
		s.log.Debug("Mini App artifact download ended early", "thread_id", id, "error", err)
	}
}

// getArtifactVersion answers the engine's version of a listed artifact: the
// SHA-256 the Mini App names in feedback, which the engine checks again on
// save, so both sides of that comparison come from one place.
func (s *server) getArtifactVersion(w http.ResponseWriter, r *http.Request, c call) {
	id, path, ok := artifactParams(w, r, c)
	if !ok {
		return
	}
	body, err := s.engine.ArtifactVersion(r.Context(), id, path)
	if err != nil {
		s.viewFailure(w, err, "Artifact versions", "Artifact version failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

// getArtifactImage serves the canonical copy of an image artifact, the
// bytes the Mini App previews and annotates, with its identity in headers.
// 422 with the engine's reason when the image cannot be annotated.
func (s *server) getArtifactImage(w http.ResponseWriter, r *http.Request, c call) {
	id, path, ok := artifactParams(w, r, c)
	if !ok {
		return
	}
	select {
	case s.imagePermits <- struct{}{}:
	case <-r.Context().Done():
		return
	}
	// The slot is held until the engine answers, even when the client left:
	// the engine keeps decoding either way.
	image, err := s.engine.ArtifactCanonical(detached(r.Context()), id, path)
	<-s.imagePermits
	if err != nil {
		s.viewFailure(w, err, "Image previews", "Image preview failed")
		return
	}
	if !servableImage(image) {
		s.log.Error("Engine answered a canonical image Go will not serve", "thread_id", id, "mime", image.Mime, "bytes", len(image.Data))
		writeText(w, http.StatusInternalServerError, "Image preview failed")
		return
	}
	h := w.Header()
	h.Set("Content-Type", image.Mime)
	h.Set("Cache-Control", "private, no-store")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Content-Security-Policy", artifactCSP)
	h.Set("X-Zdx-Source-Sha256", image.SourceSHA256)
	h.Set("X-Zdx-Canonical-Sha256", image.SHA256)
	h.Set("X-Zdx-Canonical-Width", strconv.FormatUint(uint64(image.Width), 10))
	h.Set("X-Zdx-Canonical-Height", strconv.FormatUint(uint64(image.Height), 10))
	h.Set("X-Zdx-Source-Width", strconv.FormatUint(uint64(image.SourceWidth), 10))
	h.Set("X-Zdx-Source-Height", strconv.FormatUint(uint64(image.SourceHeight), 10))
	h.Set("Content-Length", strconv.Itoa(len(image.Data)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(image.Data)
}

// servableImage is a guard on the engine's answer, not image logic: bytes
// are present, the type is a raster image (never SVG, which can carry
// script, on the app's own origin), and the identity values fit a header.
func servableImage(image CanonicalImage) bool {
	mime := strings.ToLower(image.Mime)
	return len(image.Data) > 0 &&
		strings.HasPrefix(mime, "image/") && !strings.HasPrefix(mime, "image/svg") &&
		headerValue(image.Mime) && headerValue(image.SourceSHA256) && headerValue(image.SHA256)
}

// openRegular opens a regular file without blocking on a FIFO or device:
// the type is checked before the open and again on the open file.
func openRegular(path string) (*os.File, os.FileInfo, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, nil, errNotRegular
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, nil, err
	}
	opened, err := file.Stat()
	if err != nil || !opened.Mode().IsRegular() || !os.SameFile(info, opened) {
		file.Close()
		return nil, nil, errNotRegular
	}
	return file, opened, nil
}

var errNotRegular = errors.New("not a regular file")

// headerValue is the rule of Rust `HeaderValue::from_str`: visible ASCII,
// space, tab, and bytes from 0x80 up.
func headerValue(value string) bool {
	for i := 0; i < len(value); i++ {
		c := value[i]
		if (c < 0x20 && c != '\t') || c == 0x7f {
			return false
		}
	}
	return true
}
