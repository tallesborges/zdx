// Package quota reads live subscription quotas for the flat-rate providers
// zdx logs in to, and converts them into the shapes the CLI, monitor, and
// Mini App show. It replaces the removed Rust readers
// (crates/zdx-providers/src/subscription_quota.rs at commit aab08ba4).
//
// Credentials: Fetch reads $ZDX_HOME/oauth.json. When a stored access token
// is expired or within 5 minutes of expiry, Fetch refreshes it with its
// refresh token (under oauth.lock, atomic rename) before calling the quota
// endpoint, so the monitor and `zdx quota` never need a manual re-login
// while the refresh token is valid. Muse Code re-mints its key on every
// quota fetch, so it is covered by the same invariant. The provider
// endpoints are undocumented; parsing follows the Rust readers, checked
// against the recorded Rust output in testdata/differential (known
// differences are listed in AGENTS.md). Every failure is a bounded [Error]
// that carries no response body, token, or URL.
package quota

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"sort"
	"strconv"
	"sync"
	"time"
)

// Provider ids, in the order Fetch reports them.
const (
	ProviderClaude      = "claude-cli"
	ProviderCodex       = "openai-codex"
	ProviderAntigravity = "google-antigravity"
	ProviderGrok        = "grok-build"
	ProviderMuseCode    = "muse-code"
	ProviderCopilot     = "github-copilot"
)

var providers = []string{
	ProviderClaude, ProviderCodex, ProviderAntigravity, ProviderGrok, ProviderMuseCode, ProviderCopilot,
}

// Providers lists the supported provider ids in report order.
func Providers() []string { return append([]string(nil), providers...) }

// ProviderDisplay is the display name of a provider id; an unknown id is
// returned as is.
func ProviderDisplay(provider string) string {
	switch provider {
	case ProviderClaude:
		return "Claude"
	case ProviderCodex:
		return "Codex"
	case ProviderAntigravity:
		return "Antigravity"
	case ProviderGrok:
		return "Grok"
	case ProviderMuseCode:
		return "Muse Code"
	case ProviderCopilot:
		return "Copilot"
	}
	return provider
}

// AccountDisplay labels a provider account: `Claude`, or `Claude @work` for
// a named account.
func AccountDisplay(provider string, account *string) string {
	if account == nil {
		return ProviderDisplay(provider)
	}
	return ProviderDisplay(provider) + " @" + *account
}

// Window is one quota window, such as a 5-hour session or a weekly limit.
type Window struct {
	Label string
	// UsedPercent is as the provider reported it (not clamped). A provider
	// that omits usage where the Rust reader read it as 0 gives 0 here too;
	// a window the provider does not report is absent, never 0.
	UsedPercent float64
	// ResetsAt is UTC, nil when unknown. Print it with FormatRFC3339: a
	// reset the provider sent as a leap second (`:60`) is stored as the next
	// second and only FormatRFC3339 prints it back as `:60`.
	ResetsAt *time.Time
	Scope    *string
}

// Quota is one account's quota.
type Quota struct {
	Plan    *string
	Windows []Window // never empty
}

// ErrorKind is the category of a failed account fetch.
type ErrorKind int

const (
	KindNotAuthenticated ErrorKind = iota
	KindExpired
	KindUnauthorized
	KindRateLimited
	KindTimeout
	KindHTTP
	KindIncompatible
	KindTransport
)

// Error is a bounded fetch failure for one account. It never carries a
// response body, token, or URL.
type Error struct {
	Kind           ErrorKind
	Status         int     // HTTP status, for KindHTTP
	RetryAfterSecs *uint64 // Retry-After in seconds, for KindRateLimited
}

// Reason is the text the Rust readers showed for the failure.
func (e *Error) Reason() string {
	switch e.Kind {
	case KindNotAuthenticated:
		return "not logged in"
	case KindExpired:
		return "expired · re-login in zdx"
	case KindUnauthorized:
		return "unauthorized"
	case KindRateLimited:
		return "rate limited"
	case KindTimeout:
		return "timed out"
	case KindHTTP:
		return "HTTP " + strconv.Itoa(e.Status)
	case KindIncompatible:
		return "unexpected response"
	}
	return "network error"
}

func (e *Error) Error() string { return "quota: " + e.Reason() }

func failure(kind ErrorKind) *Error { return &Error{Kind: kind} }

// Result is one provider account: exactly one of Quota and Err is set.
type Result struct {
	Provider string
	Account  *string // nil for the default account
	Quota    *Quota
	Err      *Error
}

// Snapshot is the quota of every supported account, in provider order, then
// default account first, then named accounts sorted by name. A provider
// with no stored account has one default row, which fails as not logged in.
type Snapshot struct {
	Providers []Result
}

var (
	// ErrDiscovery means the account store could not be read. The wrapped
	// cause names the file and the problem, never its contents.
	ErrDiscovery = errors.New("quota: account discovery failed")
	// ErrVersion means Options.Version is empty.
	ErrVersion = errors.New("quota: Options.Version is required")
)

// Endpoints overrides provider URLs (tests). An empty field is the
// production URL.
type Endpoints struct {
	ClaudeUsage      string
	ClaudeProfile    string
	CodexUsage       string
	AntigravityQuota string
	GrokBilling      string
	MuseCodeKey      string
	CopilotUser      string
	// Token refresh URLs.
	ClaudeToken      string
	CodexToken       string
	AntigravityToken string
	GrokToken        string
}

// Production endpoints.
const (
	ClaudeUsageURL      = "https://api.anthropic.com/api/oauth/usage"
	ClaudeProfileURL    = "https://api.anthropic.com/api/oauth/profile"
	CodexUsageURL       = "https://chatgpt.com/backend-api/wham/usage"
	AntigravityQuotaURL = "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary"
	GrokBillingURL      = "https://cli-chat-proxy.grok.com/v1/billing?format=credits"
	MuseCodeKeyURL      = "https://api.meta.ai/muse-code/key"
	CopilotUserURL      = "https://api.github.com/copilot_internal/user"
)

func (e Endpoints) withDefaults() Endpoints {
	set := func(v *string, def string) {
		if *v == "" {
			*v = def
		}
	}
	set(&e.ClaudeUsage, ClaudeUsageURL)
	set(&e.ClaudeProfile, ClaudeProfileURL)
	set(&e.CodexUsage, CodexUsageURL)
	set(&e.AntigravityQuota, AntigravityQuotaURL)
	set(&e.GrokBilling, GrokBillingURL)
	set(&e.MuseCodeKey, MuseCodeKeyURL)
	set(&e.CopilotUser, CopilotUserURL)
	return e.withTokenDefaults()
}

// Per-request limits of the Rust readers.
const (
	ConnectTimeout = 3 * time.Second
	RequestTimeout = 10 * time.Second
)

// Options are the explicit inputs of Fetch.
type Options struct {
	// Home is the ZDX home directory (oauth.json lives there). Empty means
	// resolve it as Rust does: $ZDX_HOME when set (even to ""), else
	// $HOME/.zdx, else $USERPROFILE/.zdx.
	Home string
	// HTTP sends the requests. Nil is a client with the Rust limits
	// (ConnectTimeout for the whole connection setup including TLS and a
	// proxy tunnel, RequestTimeout for the request) and reqwest's proxy
	// variables read through LookupEnv. Fetch uses a copy with its own
	// redirect policy; every client gets the MaxResponseBytes cap.
	HTTP *http.Client
	// LookupEnv reads ZDX_HOME, HOME, USERPROFILE, and,
	// for the default client, the proxy variables. Nil is os.LookupEnv.
	// Unset and empty differ, as in Rust.
	LookupEnv func(string) (string, bool)
	// Now is the clock for token expiry. Nil is time.Now.
	Now func() time.Time
	// Version is the zdx version for the `zdx/<version>` user agent (Codex,
	// Muse Code). Required.
	Version   string
	Endpoints Endpoints
}

// Account is one stored provider account (nil Account is the default one).
type Account struct {
	Provider string
	Account  *string
}

// Fetch reads the quota of every supported account concurrently. Account
// failures stay on their Result; the error is only ErrVersion, ErrDiscovery
// (oauth.json unreadable or malformed; a missing file is no accounts), or
// the context's error when ctx ends before every fetch returns.
// Expired or soon-to-expire (within 5 min) access tokens are refreshed
// first when a refresh token is present.
func Fetch(ctx context.Context, o Options) (Snapshot, error) {
	if o.Version == "" {
		return Snapshot{}, ErrVersion
	}
	f, err := newFetcher(o)
	if err != nil {
		return Snapshot{}, err
	}
	defer f.close()
	cache, err := f.loadCache()
	if err != nil {
		return Snapshot{}, err
	}
	accounts := cache.accounts()
	// Proactive refresh: never show "expired" while refresh token is valid.
	f.ensureFresh(ctx, cache, accounts)
	accounts = cache.accounts()
	results := make([]Result, len(accounts))
	var wg sync.WaitGroup
	for i, a := range accounts {
		wg.Add(1)
		go func() {
			defer wg.Done()
			q, e := f.fetch(ctx, cache, a)
			results[i] = Result{Provider: a.Provider, Account: a.Account, Quota: q, Err: e}
		}()
	}
	wg.Wait()
	if err := ctx.Err(); err != nil {
		return Snapshot{}, err
	}
	return Snapshot{Providers: results}, nil
}

// StoredAccounts lists the accounts Fetch would read, in report order.
func StoredAccounts(o Options) ([]Account, error) {
	f, err := newFetcher(o)
	if err != nil {
		return nil, err
	}
	defer f.close()
	cache, err := f.loadCache()
	if err != nil {
		return nil, err
	}
	return cache.accounts(), nil
}

type fetcher struct {
	home      string
	http      *http.Client
	ownClient bool
	lookupEnv func(string) (string, bool)
	now       func() time.Time
	version   string
	urls      Endpoints
}

func newFetcher(o Options) (*fetcher, error) {
	f := &fetcher{lookupEnv: o.LookupEnv, now: o.Now, version: o.Version, urls: o.Endpoints.withDefaults()}
	if f.lookupEnv == nil {
		f.lookupEnv = os.LookupEnv
	}
	if f.now == nil {
		f.now = time.Now
	}
	f.home = o.Home
	if f.home == "" {
		home, err := resolveHome(f.lookupEnv)
		if err != nil {
			return nil, err
		}
		f.home = home
	}
	if o.HTTP == nil {
		f.http, f.ownClient = defaultClient(f.lookupEnv), true
	} else {
		c := *o.HTTP
		f.http = &c
	}
	f.http.CheckRedirect = checkRedirect
	return f, nil
}

func (f *fetcher) close() {
	if f.ownClient {
		f.http.CloseIdleConnections()
	}
}

func (f *fetcher) fetch(ctx context.Context, cache *oauthCache, a Account) (*Quota, *Error) {
	switch a.Provider {
	case ProviderClaude:
		return f.claude(ctx, cache.get(a))
	case ProviderCodex:
		return f.codex(ctx, cache.get(a))
	case ProviderAntigravity:
		return f.antigravity(ctx, cache.get(a))
	case ProviderGrok:
		return f.grok(ctx, cache.get(a))
	case ProviderMuseCode:
		return f.museCode(ctx, cache.get(a))
	case ProviderCopilot:
		return f.copilot(ctx, cache.get(a))
	}
	return nil, failure(KindNotAuthenticated)
}

// expired is OAuthCredentials::is_expired: now in Unix milliseconds (u64
// max before the epoch) at or past the stored expiry.
func (f *fetcher) expired(c *credentials) bool {
	now := f.now()
	ms := uint64(1<<64 - 1)
	if !now.Before(time.Unix(0, 0)) {
		ms = uint64(now.UnixMilli())
	}
	return ms >= c.expires
}

func sortAccounts(names []string) []*string {
	sort.Strings(names)
	out := make([]*string, len(names))
	for i := range names {
		out[i] = &names[i]
	}
	return out
}

func discoveryError(format string, args ...any) error {
	return fmt.Errorf("%w: %s", ErrDiscovery, fmt.Sprintf(format, args...))
}
