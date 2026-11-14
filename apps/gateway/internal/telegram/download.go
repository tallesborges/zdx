package telegram

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// MaxDownloadBytes is the Bot API limit for files a bot can download.
const MaxDownloadBytes = 20 << 20

// ErrFileTooLarge means a download was stopped at its byte limit.
var ErrFileTooLarge = errors.New("file is larger than the download limit")

// ErrInvalidFilePath means a download path was refused before any request:
// it could leave the bot's file directory. Trying again cannot help.
var ErrInvalidFilePath = errors.New("invalid Telegram file path; not requested")

// errRedirect stops redirects: Bot API file URLs carry the token, and a
// redirect target is not Telegram's file server.
var errRedirect = errors.New("redirect refused")

// DownloadFile fetches a file by the path getFile returned, reading at most
// limit bytes (MaxDownloadBytes when limit <= 0). It refuses paths that could
// leave the bot's file directory, does not follow redirects, and never puts
// the URL (which holds the token) in errors or logs. A refused path is
// ErrInvalidFilePath, neither a TransportError nor an APIError, so callers do
// not retry it. A GET changes nothing, so callers may retry other failures.
func (c *Client) DownloadFile(ctx context.Context, filePath string, limit int64) ([]byte, error) {
	const method = "downloadFile"
	if limit <= 0 || limit > MaxDownloadBytes {
		limit = MaxDownloadBytes
	}
	if !validFilePath(filePath) {
		return nil, ErrInvalidFilePath
	}
	started := c.clock.Now()
	data, err := c.download(ctx, method, filePath, limit)
	c.logCall(unscopedSpec(method), 1, 0, c.clock.Now().Sub(started), nil, err)
	return data, err
}

func (c *Client) download(ctx context.Context, method, filePath string, limit int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+"/file/bot"+c.token+"/"+filePath, nil)
	if err != nil {
		return nil, fmt.Errorf("%w: building the request failed", ErrInvalidFilePath)
	}
	hc := *c.http
	hc.CheckRedirect = func(*http.Request, []*http.Request) error { return errRedirect }
	if hc.Timeout > 0 {
		// Large files take longer than an API call.
		hc.Timeout = max(hc.Timeout, 2*time.Minute)
	}
	resp, err := hc.Do(req)
	if err != nil {
		if errors.Is(err, errRedirect) {
			return nil, &APIError{Method: method, Description: "file download was redirected; refused"}
		}
		return nil, transportError(method, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, &APIError{Method: method, Code: resp.StatusCode, Description: fmt.Sprintf("file download failed with status %d", resp.StatusCode)}
	}
	if resp.ContentLength > limit {
		return nil, fmt.Errorf("%w (%d bytes > %d)", ErrFileTooLarge, resp.ContentLength, limit)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, &TransportError{Method: method, Stage: Ambiguous, Detail: "reading the file failed: " + scrub(err)}
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("%w (more than %d bytes)", ErrFileTooLarge, limit)
	}
	return data, nil
}

// validFilePath accepts the relative paths getFile returns ("photos/x.jpg")
// and nothing that could change the URL: no absolute path, no "..", no
// query, fragment, backslash, or control character.
func validFilePath(p string) bool {
	if p == "" || strings.HasPrefix(p, "/") || strings.ContainsAny(p, "?#\\%") {
		return false
	}
	for _, r := range p {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	for _, seg := range strings.Split(p, "/") {
		if seg == "" || seg == "." || seg == ".." {
			return false
		}
	}
	return true
}
