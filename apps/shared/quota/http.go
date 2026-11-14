package quota

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
)

// checkRedirect is reqwest's default policy: follow up to 10 redirects, and
// drop credentials once any hop changes scheme, host, or port. Go alone
// would keep Authorization on a redirect to a subdomain. The drop is sticky,
// as in reqwest: Go copies the first request's headers onto every hop, so
// each hop checks the whole chain, not only the last step.
func checkRedirect(req *http.Request, via []*http.Request) error {
	if len(via) > 10 {
		return errors.New("too many redirects")
	}
	for i, prev := range via {
		next := req.URL
		if i+1 < len(via) {
			next = via[i+1].URL
		}
		if next.Scheme != prev.URL.Scheme || next.Hostname() != prev.URL.Hostname() || port(next) != port(prev.URL) {
			for _, h := range []string{"Authorization", "Cookie", "Cookie2", "Proxy-Authorization", "Www-Authenticate"} {
				req.Header.Del(h)
			}
			break
		}
	}
	return nil
}

func port(u *url.URL) string {
	if p := u.Port(); p != "" {
		return p
	}
	switch u.Scheme {
	case "https":
		return "443"
	case "http":
		return "80"
	}
	return ""
}

// MaxResponseBytes caps a provider answer. Rust read bodies without a cap;
// this is hardening: a larger body is KindIncompatible. Quota answers are a
// few KiB.
const MaxResponseBytes = 8 << 20

type request struct {
	method  string
	url     string
	headers [][2]string
	body    []byte
}

// noUserAgent keeps Go from sending its own User-Agent where reqwest sent
// none.
const noUserAgent = ""

// do sends one request and returns the body of a 2xx answer. A transport
// failure is KindTimeout or KindTransport; a status is mapped as Rust's
// `error_for_status`; a body that cannot be read is KindIncompatible, as a
// failed `resp.json()` was.
func (f *fetcher) do(ctx context.Context, r request) ([]byte, *Error) {
	var body io.Reader
	if r.body != nil {
		body = bytes.NewReader(r.body)
	}
	req, err := http.NewRequestWithContext(ctx, r.method, r.url, body)
	if err != nil {
		return nil, failure(KindTransport)
	}
	hasAccept, hasUA := false, false
	for _, h := range r.headers {
		switch http.CanonicalHeaderKey(h[0]) {
		case "Accept":
			hasAccept = true
		case "User-Agent":
			hasUA = true
		}
		req.Header.Set(h[0], h[1])
	}
	if !hasAccept {
		req.Header.Set("Accept", "*/*")
	}
	if !hasUA {
		req.Header["User-Agent"] = []string{noUserAgent}
	}
	resp, err := f.http.Do(req)
	if err != nil {
		return nil, classify(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, statusError(resp)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, MaxResponseBytes+1))
	if err != nil || len(data) > MaxResponseBytes {
		return nil, failure(KindIncompatible)
	}
	return data, nil
}

func classify(err error) *Error {
	var ne net.Error
	if errors.Is(err, context.DeadlineExceeded) || (errors.As(err, &ne) && ne.Timeout()) {
		return failure(KindTimeout)
	}
	return failure(KindTransport)
}

func statusError(resp *http.Response) *Error {
	switch resp.StatusCode {
	case 401, 403:
		return failure(KindUnauthorized)
	case 429:
		return &Error{Kind: KindRateLimited, RetryAfterSecs: retryAfter(resp.Header)}
	}
	return &Error{Kind: KindHTTP, Status: resp.StatusCode}
}

// retryAfter is Rust's `parse_retry_after`: the first Retry-After value,
// visible ASCII, trimmed, as u64 seconds (a leading `+` allowed). The
// HTTP-date form is nil.
func retryAfter(h http.Header) *uint64 {
	values := h.Values("Retry-After")
	if len(values) == 0 {
		return nil
	}
	v := values[0]
	for i := 0; i < len(v); i++ {
		if c := v[i]; (c < 32 && c != '\t') || c == 127 || c > 127 {
			return nil
		}
	}
	v = strings.Trim(v, " \t\n\v\f\r")
	if strings.HasPrefix(v, "+") {
		v = v[1:]
		if v == "" || v[0] == '+' || v[0] == '-' {
			return nil
		}
	}
	n, err := strconv.ParseUint(v, 10, 64)
	if err != nil {
		return nil
	}
	return &n
}
