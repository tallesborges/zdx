package quota

import (
	"bufio"
	"context"
	"crypto/x509"
	"encoding/base64"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

// tlsProvider is an HTTPS fake with HTTP/2 enabled.
func tlsProvider(t *testing.T, h http.HandlerFunc) (*httptest.Server, *x509.CertPool) {
	t.Helper()
	srv := httptest.NewUnstartedServer(h)
	srv.Config.ErrorLog = log.New(io.Discard, "", 0)
	srv.EnableHTTP2 = true
	srv.StartTLS()
	t.Cleanup(srv.Close)
	pool := x509.NewCertPool()
	pool.AddCert(srv.Certificate())
	return srv, pool
}

func grokOnly(t *testing.T) string {
	home := t.TempDir()
	writeCache(t, home, map[string]cred{"grok-build": {Type: "oauth", Refresh: "r", Access: "tok-grok-SECRET", Expires: future}})
	return home
}

func fetchGrok(t *testing.T, home, url string, client *http.Client, vars map[string]string) Result {
	t.Helper()
	o := options(home, nil, vars)
	o.Endpoints.GrokBilling = url
	o.HTTP = client
	snap, err := Fetch(context.Background(), o)
	if err != nil {
		t.Fatal(err)
	}
	return snap.Providers[3]
}

func TestDefaultTransportSpeaksTLSAndHTTP2(t *testing.T) {
	var proto string
	srv, pool := tlsProvider(t, func(w http.ResponseWriter, r *http.Request) {
		proto = r.Proto
		io.WriteString(w, `{"config":{"creditUsagePercent":5}}`)
	})
	client := newClient(transportConfig{proxies: proxyFromEnv(env(nil)), connect: time.Second, total: 5 * time.Second, rootCAs: pool})
	r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil)
	if r.Err != nil || r.Quota.Windows[0].UsedPercent != 5 {
		t.Fatalf("%+v", r)
	}
	if proto != "HTTP/2.0" {
		t.Errorf("proto = %s, want HTTP/2.0 (ALPN through the custom TLS dialer)", proto)
	}
	// A certificate the pool does not trust fails as a network error.
	untrusted := newClient(transportConfig{proxies: proxyFromEnv(env(nil)), connect: time.Second, total: 5 * time.Second})
	if r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", untrusted, nil); r.Err == nil || r.Err.Kind != KindTransport {
		t.Errorf("untrusted = %+v", r.Err)
	}
}

// stallingListener accepts connections and never answers (a TLS handshake
// that hangs).
func stallingListener(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	var conns []net.Conn
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			mu.Lock()
			conns = append(conns, c)
			mu.Unlock()
		}
	}()
	t.Cleanup(func() {
		ln.Close()
		mu.Lock()
		for _, c := range conns {
			c.Close()
		}
		mu.Unlock()
	})
	return ln.Addr().String()
}

func TestConnectBudgetCoversDialAndTLSTogether(t *testing.T) {
	addr := stallingListener(t)
	const budget = time.Second
	slowDial := func(ctx context.Context, network, a string) (net.Conn, error) {
		select {
		case <-time.After(700 * time.Millisecond):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		return (&net.Dialer{}).DialContext(ctx, network, a)
	}
	client := newClient(transportConfig{proxies: proxyFromEnv(env(nil)), connect: budget, total: 10 * time.Second, dial: slowDial})
	start := time.Now()
	r := fetchGrok(t, grokOnly(t), "https://"+addr+"/grok", client, nil)
	elapsed := time.Since(start)
	if r.Err == nil || r.Err.Kind != KindTimeout {
		t.Fatalf("err = %+v, want timed out", r.Err)
	}
	// Separate budgets would allow 0.7 s dial + 1 s TLS = 1.7 s.
	if elapsed < budget-50*time.Millisecond || elapsed > budget+400*time.Millisecond {
		t.Errorf("elapsed = %v, want about %v", elapsed, budget)
	}
	// A dial that never connects is also cut at the budget.
	blocked := func(ctx context.Context, _, _ string) (net.Conn, error) { <-ctx.Done(); return nil, ctx.Err() }
	client = newClient(transportConfig{proxies: proxyFromEnv(env(nil)), connect: 200 * time.Millisecond, total: 10 * time.Second, dial: blocked})
	if r := fetchGrok(t, grokOnly(t), "https://quota.test/grok", client, nil); r.Err == nil || r.Err.Kind != KindTimeout {
		t.Errorf("blocked dial = %+v", r.Err)
	}
}

func TestDefaultClientBudgets(t *testing.T) {
	c := defaultClient(env(nil))
	if c.Timeout != RequestTimeout || ConnectTimeout != 3*time.Second || RequestTimeout != 10*time.Second {
		t.Errorf("timeouts = %v %v %v", c.Timeout, ConnectTimeout, RequestTimeout)
	}
	tr := c.Transport.(*http.Transport)
	if !tr.DisableCompression || tr.DialTLSContext == nil || tr.TLSHandshakeTimeout != 0 {
		t.Errorf("transport = %+v", tr)
	}
}

// connectProxy is a fake HTTP CONNECT proxy that records each request and
// tunnels to the requested address (or answers status instead).
type connectProxy struct {
	addr        string
	mu          sync.Mutex
	seen        []*http.Request
	status      int
	established string // the 200 answer's head
}

func newConnectProxy(t *testing.T, status int) *connectProxy {
	return newConnectProxyAnswering(t, status, "HTTP/1.1 200 Connection established\r\n\r\n")
}

func newConnectProxyAnswering(t *testing.T, status int, established string) *connectProxy {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	p := &connectProxy{addr: ln.Addr().String(), status: status, established: established}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go p.handle(c)
		}
	}()
	return p
}

func (p *connectProxy) handle(c net.Conn) {
	defer c.Close()
	req, err := http.ReadRequest(bufio.NewReader(c))
	if err != nil {
		return
	}
	p.mu.Lock()
	p.seen = append(p.seen, req)
	p.mu.Unlock()
	if p.status != 200 {
		io.WriteString(c, "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n")
		return
	}
	up, err := net.Dial("tcp", req.Host)
	if err != nil {
		return
	}
	defer up.Close()
	io.WriteString(c, p.established)
	go io.Copy(up, c)
	io.Copy(c, up)
}

func (p *connectProxy) requests() []*http.Request {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]*http.Request(nil), p.seen...)
}

func TestHTTPSProxyTunnelFromEnvironment(t *testing.T) {
	srv, pool := tlsProvider(t, func(w http.ResponseWriter, r *http.Request) {
		io.WriteString(w, `{"config":{"creditUsagePercent":7}}`)
	})
	target := strings.TrimPrefix(srv.URL, "https://")
	run := func(vars map[string]string) (Result, *connectProxy) {
		p := newConnectProxy(t, 200)
		for k, v := range vars {
			vars[k] = strings.ReplaceAll(v, "PROXY", p.addr)
		}
		client := newClient(transportConfig{proxies: proxyFromEnv(env(vars)), connect: 2 * time.Second, total: 5 * time.Second, rootCAs: pool})
		return fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil), p
	}
	for name, c := range map[string]struct {
		vars    map[string]string
		proxied bool
	}{
		"HTTPS_PROXY":     {map[string]string{"HTTPS_PROXY": "http://PROXY"}, true},
		"https_proxy":     {map[string]string{"https_proxy": "PROXY"}, true},
		"ALL_PROXY":       {map[string]string{"ALL_PROXY": "http://PROXY"}, true},
		"all_proxy":       {map[string]string{"all_proxy": "http://PROXY"}, true},
		"HTTP_PROXY only": {map[string]string{"HTTP_PROXY": "http://PROXY"}, false},
		"NO_PROXY ip":     {map[string]string{"HTTPS_PROXY": "http://PROXY", "NO_PROXY": "example.test, 127.0.0.1"}, false},
		"NO_PROXY net":    {map[string]string{"ALL_PROXY": "http://PROXY", "no_proxy": "127.0.0.0/8"}, false},
		// hyper-util matches IP hosts only against IP entries, so `*` does
		// not bypass the proxy for an IP target.
		"NO_PROXY star, IP host":  {map[string]string{"ALL_PROXY": "http://PROXY", "NO_PROXY": "*"}, true},
		"CGI":                     {map[string]string{"HTTPS_PROXY": "http://PROXY", "REQUEST_METHOD": "GET"}, false},
		"upper wins even empty":   {map[string]string{"HTTPS_PROXY": "", "https_proxy": "http://PROXY"}, false},
		"unsupported scheme":      {map[string]string{"HTTPS_PROXY": "ftp://PROXY"}, false},
		"HTTPS_PROXY over ALL":    {map[string]string{"HTTPS_PROXY": "http://PROXY", "ALL_PROXY": "http://127.0.0.1:1"}, true},
		"unparsable falls to ALL": {map[string]string{"HTTPS_PROXY": "http://[bad", "ALL_PROXY": "http://PROXY"}, true},
	} {
		r, p := run(c.vars)
		if r.Err != nil {
			t.Errorf("%s: %v", name, r.Err)
			continue
		}
		reqs := p.requests()
		if c.proxied != (len(reqs) == 1) {
			t.Errorf("%s: proxy requests = %d", name, len(reqs))
			continue
		}
		if c.proxied {
			req := reqs[0]
			if req.Method != "CONNECT" || req.Host != target || req.Header.Get("Authorization") != "" || req.Header.Get("Proxy-Authorization") != "" {
				t.Errorf("%s: CONNECT = %s %s %v", name, req.Method, req.Host, req.Header)
			}
		}
	}

	// Basic auth from the proxy URL; the provider token never reaches the proxy.
	r, p := run(map[string]string{"HTTPS_PROXY": "http://us%3Aer:p%40ss@PROXY"})
	if r.Err != nil {
		t.Fatal(r.Err)
	}
	want := "Basic " + base64.StdEncoding.EncodeToString([]byte("us:er:p@ss"))
	if got := p.requests()[0].Header.Get("Proxy-Authorization"); got != want {
		t.Errorf("proxy auth = %q, want %q", got, want)
	}

	// A 200 with a Content-Length is still a tunnel: the client must not
	// wait for a body before its TLS handshake (hyper-util reads only the
	// head).
	framed := newConnectProxyAnswering(t, 200, "HTTP/1.1 200 Connection established\r\nContent-Length: 1\r\n\r\n")
	client := newClient(transportConfig{proxies: proxyFromEnv(env(map[string]string{"HTTPS_PROXY": "http://" + framed.addr})), connect: time.Second, total: 5 * time.Second, rootCAs: pool})
	if r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil); r.Err != nil {
		t.Errorf("framed 200 tunnel = %+v", r.Err)
	}

	// A refused tunnel is a network error and names nothing.
	refusing := newConnectProxy(t, 407)
	client = newClient(transportConfig{proxies: proxyFromEnv(env(map[string]string{"HTTPS_PROXY": "http://" + refusing.addr})), connect: time.Second, total: 5 * time.Second, rootCAs: pool})
	if r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil); r.Err == nil || r.Err.Kind != KindTransport {
		t.Errorf("refused tunnel = %+v", r.Err)
	}
	// A socks proxy fails the connection (the Rust build has no socks support).
	client = newClient(transportConfig{proxies: proxyFromEnv(env(map[string]string{"ALL_PROXY": "socks5://127.0.0.1:1"})), connect: time.Second, total: 5 * time.Second, rootCAs: pool})
	if r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil); r.Err == nil || r.Err.Kind != KindTransport {
		t.Errorf("socks = %+v", r.Err)
	}
	// A proxy that accepts and never answers is cut by the connect budget.
	stall := stallingListener(t)
	client = newClient(transportConfig{proxies: proxyFromEnv(env(map[string]string{"HTTPS_PROXY": "http://" + stall})), connect: 300 * time.Millisecond, total: 5 * time.Second, rootCAs: pool})
	start := time.Now()
	if r := fetchGrok(t, grokOnly(t), srv.URL+"/grok", client, nil); r.Err == nil || r.Err.Kind != KindTimeout {
		t.Errorf("stalled proxy = %+v", r.Err)
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Errorf("stalled proxy took %v", elapsed)
	}
}

func TestPlainHTTPProxy(t *testing.T) {
	var seenURI, seenAuth string
	proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seenURI, seenAuth = r.RequestURI, r.Header.Get("Authorization")
		io.WriteString(w, `{"config":{"creditUsagePercent":9}}`)
	}))
	t.Cleanup(proxy.Close)
	client := newClient(transportConfig{proxies: proxyFromEnv(env(map[string]string{"ALL_PROXY": proxy.URL})), connect: time.Second, total: 5 * time.Second})
	r := fetchGrok(t, grokOnly(t), "http://provider.test/grok", client, nil)
	if r.Err != nil || r.Quota.Windows[0].UsedPercent != 9 {
		t.Fatalf("%+v", r)
	}
	if seenURI != "http://provider.test/grok" || seenAuth != "Bearer tok-grok-SECRET" {
		t.Errorf("proxy saw %q %q", seenURI, seenAuth)
	}
}

func TestNoProxyMatchesHyperUtil(t *testing.T) {
	n := parseNoProxy(" google.com , .yahoo.com, 192.168.1.0/24, ::1, 10.0.0.1 ,, ")
	for host, want := range map[string]bool{
		"google.com": true, "www.google.com": true, "GOOGLE.COM": true, "notgoogle.com": false,
		"yahoo.com": true, "a.yahoo.com": true, "ayahoo.com": false,
		"192.168.1.42": true, "192.168.2.1": false, "[::1]": true, "::1": true, "10.0.0.1": true, "10.0.0.2": false,
		"example.com": false,
	} {
		if got := n.contains(host); got != want {
			t.Errorf("%s = %v", host, got)
		}
	}
	if !parseNoProxy("*").contains("anything.test") || parseNoProxy("*").contains("127.0.0.1") || parseNoProxy("").contains("x") {
		t.Error("wildcard / empty")
	}
	// Non-ASCII case folding is not applied (Rust eq_ignore_ascii_case).
	if parseNoProxy("\u212aelvin.test").contains("kelvin.test") {
		t.Error("unicode folding")
	}
}

func TestResponseSizeCap(t *testing.T) {
	home := grokOnly(t)
	body := func(n int) string {
		head := `{"config":{"creditUsagePercent":3},"pad":"`
		return head + strings.Repeat("x", n-len(head)-2) + `"}`
	}
	for size, ok := range map[int]bool{MaxResponseBytes: true, MaxResponseBytes + 1: false} {
		f := newFake(t, map[string]http.HandlerFunc{"/grok": serve(body(size))})
		o := options(home, f, nil)
		snap, err := Fetch(context.Background(), o)
		if err != nil {
			t.Fatal(err)
		}
		r := snap.Providers[3]
		if ok != (r.Err == nil) || (!ok && r.Err.Kind != KindIncompatible) {
			t.Errorf("size %d: %+v", size, r.Err)
		}
	}
}
