package quota

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Token refresh endpoints (production). Endpoints override for tests.
const (
	ClaudeTokenURL      = "https://console.anthropic.com/v1/oauth/token"
	CodexTokenURL       = "https://auth.openai.com/oauth/token"
	AntigravityTokenURL = "https://oauth2.googleapis.com/token"
	GrokTokenURL        = "https://auth.x.ai/oauth2/token"
)

// withDefaults adds token URLs.
func (e Endpoints) withTokenDefaults() Endpoints {
	set := func(v *string, def string) {
		if *v == "" {
			*v = def
		}
	}
	set(&e.ClaudeToken, ClaudeTokenURL)
	set(&e.CodexToken, CodexTokenURL)
	set(&e.AntigravityToken, AntigravityTokenURL)
	set(&e.GrokToken, GrokTokenURL)
	return e
}

// refreshWindow is how close to expiry we proactively refresh (5 min = Rust buffer).
const refreshWindow = 5 * time.Minute

func (f *fetcher) needsRefresh(c *credentials) bool {
	if c == nil {
		return false
	}
	if c.expires == 0 {
		return false
	}
	// Muse Code has NO_EXPIRY (u64::MAX) -> never refresh.
	if c.expires == ^uint64(0) {
		return false
	}
	now := f.now()
	if now.Before(time.Unix(0, 0)) {
		return true
	}
	nowMs := uint64(now.UnixMilli())
	// If already expired, or expires within refreshWindow.
	if nowMs >= c.expires {
		return true
	}
	remain := int64(c.expires) - int64(nowMs)
	if remain < 0 {
		return true
	}
	return time.Duration(remain)*time.Millisecond < refreshWindow
}

// ensureFresh attempts to refresh expired / soon-expiring credentials before quota fetch.
// It acquires oauth.lock, re-reads cache, and persists refreshed tokens atomically.
// Errors are swallowed: caller still reports KindExpired if refresh fails.
// Muse Code is excluded here because its quota fetch already re-mints the
// Model API key on every call (the mint is the refresh); it has no OAuth
// refresh-token flow to extend.
func (f *fetcher) ensureFresh(ctx context.Context, cache *oauthCache, accounts []Account) {
	for _, a := range accounts {
		// Only OAuth providers with refresh tokens need refresh.
		switch a.Provider {
		case ProviderClaude, ProviderCodex, ProviderAntigravity, ProviderGrok:
		default:
			continue
		}
		c := cache.get(a)
		if c == nil {
			continue
		}
		if !f.needsRefresh(c) {
			continue
		}
		// Attempt refresh; on success cache map is updated in place.
		f.refreshAccount(ctx, cache, a)
	}
}

func (f *fetcher) refreshAccount(ctx context.Context, cache *oauthCache, a Account) {
	if c := cache.get(a); c == nil || !f.needsRefresh(c) {
		return
	}
	unlock, err := f.acquireLock()
	if err != nil {
		return
	}
	defer unlock()
	// Re-read under lock: another process may have already refreshed.
	freshCache, err := f.loadCacheLocked()
	if err != nil {
		return
	}
	// If the reloaded cache differs, use it to avoid clobber.
	current := freshCache.get(a)
	if current == nil || !f.needsRefresh(current) {
		// No longer needs refresh, or gone. Sync cache map from fresh.
		syncCache(cache, freshCache)
		return
	}
	refreshed, err := f.doRefresh(ctx, a, current)
	if err != nil {
		return
	}
	if refreshed.accountID == nil {
		refreshed.accountID = current.accountID
	}
	freshCache.entries[cacheKey(a.Provider, a.Account)] = refreshed
	if err := f.saveCacheLocked(freshCache); err != nil {
		return
	}
	// Update the in-memory cache the caller uses.
	syncCache(cache, freshCache)
}

func syncCache(dst, src *oauthCache) {
	if src == nil {
		return
	}
	dst.entries = src.entries
}

func (f *fetcher) doRefresh(ctx context.Context, a Account, current *credentials) (*credentials, error) {
	switch a.Provider {
	case ProviderClaude:
		return f.refreshClaude(ctx, current.refresh)
	case ProviderCodex:
		return f.refreshCodex(ctx, current.refresh)
	case ProviderAntigravity:
		project := ""
		if current.accountID != nil {
			project = *current.accountID
		}
		out, err := f.refreshAntigravity(ctx, current.refresh, project)
		if err != nil {
			return nil, err
		}
		if out.accountID == nil && current.accountID != nil {
			out.accountID = current.accountID
		}
		return out, nil
	case ProviderGrok:
		return f.refreshGrok(ctx, current.refresh)
	}
	return nil, fmt.Errorf("no refresh for %s", a.Provider)
}

func computeExpires(now time.Time, expiresInSecs uint64) uint64 {
	nowMs := uint64(now.UnixMilli())
	// Rust: now + expires_in*1000 - 5*60*1000 (saturating)
	expMs := expiresInSecs * 1000
	buffer := uint64(5 * 60 * 1000)
	if expMs < buffer {
		return nowMs
	}
	return nowMs + expMs - buffer
}

// --- Per-provider refresh implementations (ports of Rust refresh_token) ---

type tokenResponse struct {
	AccessToken  string  `json:"access_token"`
	RefreshToken *string `json:"refresh_token"`
	ExpiresIn    uint64  `json:"expires_in"`
}

func (f *fetcher) refreshClaude(ctx context.Context, refreshToken string) (*credentials, error) {
	payload := map[string]string{
		"grant_type":    "refresh_token",
		"client_id":     "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
		"refresh_token": refreshToken,
	}
	body, _ := json.Marshal(payload)
	req, err := http.NewRequestWithContext(ctx, "POST", f.urls.ClaudeToken, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := f.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := readLimited(resp)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("refresh failed %d", resp.StatusCode)
	}
	var tr tokenResponse
	if err := json.Unmarshal(data, &tr); err != nil {
		return nil, err
	}
	refresh := tr.AccessToken // not used
	_ = refresh
	newRefresh := refreshToken
	if tr.RefreshToken != nil && *tr.RefreshToken != "" {
		newRefresh = *tr.RefreshToken
	}
	expires := computeExpires(f.now(), tr.ExpiresIn)
	return &credentials{refresh: newRefresh, access: tr.AccessToken, expires: expires, accountID: nil}, nil
}

func (f *fetcher) refreshCodex(ctx context.Context, refreshToken string) (*credentials, error) {
	form := url.Values{}
	form.Set("grant_type", "refresh_token")
	form.Set("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
	form.Set("refresh_token", refreshToken)
	req, err := http.NewRequestWithContext(ctx, "POST", f.urls.CodexToken, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := f.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := readLimited(resp)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("refresh failed %d", resp.StatusCode)
	}
	var tr tokenResponse
	if err := json.Unmarshal(data, &tr); err != nil {
		return nil, err
	}
	newRefresh := refreshToken
	if tr.RefreshToken != nil && *tr.RefreshToken != "" {
		newRefresh = *tr.RefreshToken
	}
	expires := computeExpires(f.now(), tr.ExpiresIn)
	acctID := decodeAccountID(tr.AccessToken)
	return &credentials{refresh: newRefresh, access: tr.AccessToken, expires: expires, accountID: acctID}, nil
}

func decodeAccountID(token string) *string {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return nil
	}
	payload := parts[1]
	// base64 URL without padding
	decoded, err := base64.RawURLEncoding.DecodeString(payload)
	if err != nil {
		// try with padding
		if m := len(payload) % 4; m != 0 {
			payload += strings.Repeat("=", 4-m)
		}
		decoded, err = base64.URLEncoding.DecodeString(payload)
		if err != nil {
			return nil
		}
	}
	var claims map[string]any
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return nil
	}
	// Rust path: claims["https://api.openai.com/auth"]["chatgpt_account_id"]
	auth, ok := claims["https://api.openai.com/auth"].(map[string]any)
	if !ok {
		return nil
	}
	id, ok := auth["chatgpt_account_id"].(string)
	if !ok || id == "" {
		return nil
	}
	return &id
}

func (f *fetcher) refreshAntigravity(ctx context.Context, refreshToken, project string) (*credentials, error) {
	form := url.Values{}
	form.Set("grant_type", "refresh_token")
	form.Set("client_id", "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com")
	form.Set("client_secret", "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")
	form.Set("refresh_token", refreshToken)
	req, err := http.NewRequestWithContext(ctx, "POST", f.urls.AntigravityToken, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := f.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := readLimited(resp)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("refresh failed %d", resp.StatusCode)
	}
	var tr tokenResponse
	if err := json.Unmarshal(data, &tr); err != nil {
		return nil, err
	}
	newRefresh := refreshToken
	if tr.RefreshToken != nil && *tr.RefreshToken != "" {
		newRefresh = *tr.RefreshToken
	}
	expires := computeExpires(f.now(), tr.ExpiresIn)
	acct := (*string)(nil)
	if project != "" {
		p := project
		acct = &p
	}
	return &credentials{refresh: newRefresh, access: tr.AccessToken, expires: expires, accountID: acct}, nil
}

func (f *fetcher) refreshGrok(ctx context.Context, refreshToken string) (*credentials, error) {
	form := url.Values{}
	form.Set("grant_type", "refresh_token")
	form.Set("client_id", "b1a00492-073a-47ea-816f-4c329264a828")
	form.Set("refresh_token", refreshToken)
	req, err := http.NewRequestWithContext(ctx, "POST", f.urls.GrokToken, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")
	resp, err := f.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := readLimited(resp)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("refresh failed %d", resp.StatusCode)
	}
	var tr struct {
		AccessToken  string  `json:"access_token"`
		RefreshToken *string `json:"refresh_token"`
		ExpiresIn    *uint64 `json:"expires_in"`
	}
	if err := json.Unmarshal(data, &tr); err != nil {
		return nil, err
	}
	newRefresh := refreshToken
	if tr.RefreshToken != nil && *tr.RefreshToken != "" {
		newRefresh = *tr.RefreshToken
	}
	secs := uint64(3600)
	if tr.ExpiresIn != nil {
		secs = *tr.ExpiresIn
	}
	expires := computeExpires(f.now(), secs)
	return &credentials{refresh: newRefresh, access: tr.AccessToken, expires: expires, accountID: nil}, nil
}

func readLimited(resp *http.Response) ([]byte, error) {
	// Cap like MaxResponseBytes (8 MiB) but keep simple: read up to 1MiB, enough for token response.
	b, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, err
	}
	return b, nil
}

// --- File locking (oauth.lock) mirroring Rust CacheLock ---

func (f *fetcher) acquireLock() (func(), error) {
	path := filepath.Join(f.home, "oauth.lock")
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	deadline := time.Now().Add(30 * time.Second)
	for {
		locked, err := tryFlock(file)
		if err != nil {
			file.Close()
			return nil, err
		}
		if locked {
			return func() {
				unlockFlock(file)
				file.Close()
			}, nil
		}
		if time.Now().After(deadline) {
			file.Close()
			return nil, fmt.Errorf("timeout waiting for oauth.lock")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// loadCacheLocked reads cache assuming lock is held or not; same as loadCache but without extra checks.
func (f *fetcher) loadCacheLocked() (*oauthCache, error) {
	// Reuse loadCache logic; it already reads file.
	return f.loadCache()
}

func (f *fetcher) saveCacheLocked(cache *oauthCache) error {
	path := filepath.Join(f.home, "oauth.json")
	// Marshal entries back to oauth.json format.
	out := map[string]map[string]any{}
	for k, v := range cache.entries {
		entry := map[string]any{
			"type":    "oauth",
			"refresh": v.refresh,
			"access":  v.access,
			"expires": v.expires,
		}
		if v.accountID != nil {
			entry["account_id"] = *v.accountID
		}
		out[k] = entry
	}
	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		return err
	}
	// Atomic write via temp file + rename, 0600.
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, "oauth.tmp.*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmpName, path); err != nil {
		return err
	}
	return nil
}
