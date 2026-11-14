package quota

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

const oauthCacheFile = "oauth.json"

// resolveHome is the Rust OAuth store's `zdx_home()`.
func resolveHome(lookupEnv func(string) (string, bool)) (string, error) {
	if home, ok := lookupEnv("ZDX_HOME"); ok {
		return home, nil
	}
	if home, ok := lookupEnv("HOME"); ok {
		return filepath.Join(home, ".zdx"), nil
	}
	if home, ok := lookupEnv("USERPROFILE"); ok {
		return filepath.Join(home, ".zdx"), nil
	}
	return "", discoveryError("could not determine home directory (ZDX_HOME and HOME are unset)")
}

// credentials is one oauth.json entry (Rust OAuthCredentials).
type credentials struct {
	refresh   string
	access    string
	expires   uint64
	accountID *string
}

// oauthCache is oauth.json: cache key (`provider` or `provider@account`) to
// credentials.
type oauthCache struct {
	entries map[string]*credentials
}

// loadCache reads oauth.json once, read-only. A missing file (or a dangling
// link) is an empty store. Any other failure, such as a permission error on
// the file or its directory, is ErrDiscovery: Rust's `Path::exists` read it
// as no accounts, which hid an unreadable store behind "not logged in".
func (f *fetcher) loadCache() (*oauthCache, error) {
	path := filepath.Join(f.home, oauthCacheFile)
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return &oauthCache{entries: map[string]*credentials{}}, nil
	}
	if err != nil {
		var pe *os.PathError
		if errors.As(err, &pe) {
			err = pe.Err
		}
		return nil, discoveryError("failed to read OAuth cache from %s: %v", path, err)
	}
	cache, err := parseCache(data)
	if err != nil {
		return nil, discoveryError("failed to parse OAuth cache from %s: %v", path, err)
	}
	return cache, nil
}

var credentialFields = []string{"type", "refresh", "access", "expires", "account_id"}

// parseCache decodes oauth.json as serde did: a JSON object whose every
// value must be a complete credentials entry, or the whole file fails. A
// repeated top-level key keeps the last value. Errors name the entry and
// field, never a value.
func parseCache(data []byte) (*oauthCache, error) {
	// read_to_string needs UTF-8 everywhere; serde flatten buffers the whole
	// file, so every string, number, and nesting level is checked.
	if !utf8.Valid(data) {
		return nil, errors.New("not valid UTF-8")
	}
	doc, err := document(data)
	if err != nil {
		return nil, errors.New("not valid JSON")
	}
	if len(doc) == 0 || doc[0] != '{' {
		return nil, errors.New("expected a JSON object")
	}
	if err := checkValue(doc, 0); err != nil {
		return nil, errors.New(strings.TrimPrefix(err.Error(), errWire.Error()+": "))
	}
	raw := map[string]json.RawMessage{}
	if err := json.Unmarshal(doc, &raw); err != nil {
		return nil, errors.New("expected a JSON object")
	}
	cache := &oauthCache{entries: map[string]*credentials{}}
	for key, value := range raw {
		c, err := parseCredentials(value)
		if err != nil {
			return nil, errors.New("entry " + quoteKey(key) + ": " + err.Error())
		}
		cache.entries[key] = c
	}
	return cache, nil
}

func parseCredentials(raw json.RawMessage) (*credentials, error) {
	o, err := object(raw, credentialFields...)
	if err != nil {
		if errors.Is(err, errWire) && strings.Contains(err.Error(), "duplicate field") {
			return nil, errors.New(strings.TrimPrefix(err.Error(), errWire.Error()+": "))
		}
		return nil, errors.New("expected an object")
	}
	if _, err := o.requiredString("type"); err != nil {
		return nil, err
	}
	refresh, err := o.requiredString("refresh")
	if err != nil {
		return nil, err
	}
	access, err := o.requiredString("access")
	if err != nil {
		return nil, err
	}
	expires, err := o.requiredUint64("expires")
	if err != nil {
		return nil, err
	}
	accountID, err := o.optString("account_id")
	if err != nil {
		return nil, errors.New("field `account_id`: expected a string")
	}
	return &credentials{refresh: refresh, access: access, expires: expires, accountID: accountID}, nil
}

// quoteKey shows a cache key in an error. Keys are provider and account
// names, not secrets; long or odd ones are cut.
func quoteKey(key string) string {
	if len(key) > 64 {
		key = key[:64] + "..."
	}
	return strings.ToValidUTF8("'"+key+"'", "?")
}

// splitKey is Rust's `split_cache_key`: the first `@` separates a non-empty
// account name.
func splitKey(key string) (string, *string) {
	provider, account, ok := strings.Cut(key, "@")
	if !ok || account == "" {
		return key, nil
	}
	return provider, &account
}

func cacheKey(provider string, account *string) string {
	if account == nil {
		return provider
	}
	return provider + "@" + *account
}

// accounts is Rust's `stored_accounts`: per provider, the default account
// first, then named accounts sorted bytewise; a provider with none gets its
// default account.
func (c *oauthCache) accounts() []Account {
	var out []Account
	for _, provider := range providers {
		hasDefault := false
		var named []string
		for key := range c.entries {
			p, account := splitKey(key)
			if p != provider {
				continue
			}
			if account == nil {
				hasDefault = true
			} else {
				named = append(named, *account)
			}
		}
		if !hasDefault && len(named) == 0 {
			out = append(out, Account{Provider: provider})
			continue
		}
		if hasDefault {
			out = append(out, Account{Provider: provider})
		}
		for _, name := range sortAccounts(named) {
			out = append(out, Account{Provider: provider, Account: name})
		}
	}
	return out
}

func (c *oauthCache) get(a Account) *credentials {
	return c.entries[cacheKey(a.Provider, a.Account)]
}
