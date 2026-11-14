package quota

import (
	"bufio"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strings"
	"time"
	"unicode/utf8"
)

// transportConfig builds the default client. Tests set rootCAs and dial.
type transportConfig struct {
	proxies *proxyEnv
	connect time.Duration // one budget for TCP connect, proxy CONNECT, and TLS
	total   time.Duration
	rootCAs *x509.CertPool
	dial    func(ctx context.Context, network, addr string) (net.Conn, error)
}

// newClient is the client Fetch uses when Options.HTTP is nil. Like
// reqwest's connect_timeout, one budget covers the whole connection setup
// of an HTTPS request: TCP connect, the proxy tunnel when one is used, and
// the TLS handshake. The total budget covers the request and body.
// Compression stays off, as reqwest without its gzip feature sends no
// Accept-Encoding.
func newClient(c transportConfig) *http.Client {
	if c.dial == nil {
		c.dial = (&net.Dialer{}).DialContext
	}
	dial := func(ctx context.Context, network, addr string) (net.Conn, error) {
		ctx, cancel := context.WithTimeout(ctx, c.connect)
		defer cancel()
		return c.dial(ctx, network, addr)
	}
	return &http.Client{
		Timeout: c.total,
		Transport: &http.Transport{
			// HTTPS targets never reach Proxy: DialTLSContext tunnels them
			// itself, so the tunnel shares the connect budget.
			Proxy:       c.proxies.forPlainHTTP,
			DialContext: dial,
			DialTLSContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
				return c.dialTLS(ctx, network, addr)
			},
			ForceAttemptHTTP2:  true,
			DisableCompression: true,
		},
	}
}

func defaultClient(lookupEnv func(string) (string, bool)) *http.Client {
	return newClient(transportConfig{proxies: proxyFromEnv(lookupEnv), connect: ConnectTimeout, total: RequestTimeout})
}

func (c transportConfig) dialTLS(ctx context.Context, network, addr string) (net.Conn, error) {
	ctx, cancel := context.WithTimeout(ctx, c.connect)
	defer cancel()
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return nil, err
	}
	var conn net.Conn
	if proxy := c.proxies.forHost("https", host); proxy != nil {
		conn, err = c.tunnel(ctx, network, proxy, addr)
	} else {
		conn, err = c.dial(ctx, network, addr)
	}
	if err != nil {
		return nil, err
	}
	tc := tls.Client(conn, &tls.Config{ServerName: host, NextProtos: []string{"h2", "http/1.1"}, RootCAs: c.rootCAs})
	if err := tc.HandshakeContext(ctx); err != nil {
		conn.Close()
		return nil, err
	}
	return tc, nil
}

// tunnel opens an HTTP CONNECT tunnel to addr through proxy, as hyper-util's
// Tunnel does for reqwest: TLS to an https proxy, Basic auth from the proxy
// URL, no other headers. Any answer but 200 fails the connection.
func (c transportConfig) tunnel(ctx context.Context, network string, proxy *proxyTarget, addr string) (net.Conn, error) {
	if proxy.scheme != "http" && proxy.scheme != "https" {
		return nil, errors.New("proxy scheme not supported")
	}
	conn, err := c.dial(ctx, network, proxy.addr)
	if err != nil {
		return nil, err
	}
	if proxy.scheme == "https" {
		tc := tls.Client(conn, &tls.Config{ServerName: proxy.host, RootCAs: c.rootCAs})
		if err := tc.HandshakeContext(ctx); err != nil {
			conn.Close()
			return nil, err
		}
		conn = tc
	}
	if deadline, ok := ctx.Deadline(); ok {
		conn.SetDeadline(deadline)
	}
	stop := context.AfterFunc(ctx, func() { conn.SetDeadline(time.Unix(1, 0)) })
	fail := func(err error) (net.Conn, error) {
		stop()
		conn.Close()
		if ctxErr := ctx.Err(); ctxErr != nil {
			return nil, ctxErr
		}
		// The connection deadline is the budget's deadline and may fire
		// just before the context's own timer.
		var ne net.Error
		if errors.As(err, &ne) && ne.Timeout() {
			return nil, context.DeadlineExceeded
		}
		return nil, err
	}
	req := "CONNECT " + addr + " HTTP/1.1\r\nHost: " + addr + "\r\n"
	if proxy.auth != "" {
		req += "Proxy-Authorization: " + proxy.auth + "\r\n"
	}
	if _, err := conn.Write([]byte(req + "\r\n")); err != nil {
		return fail(err)
	}
	br := bufio.NewReader(conn)
	resp, err := http.ReadResponse(br, &http.Request{Method: "CONNECT"})
	if err != nil {
		return fail(err)
	}
	// The body is not read or closed: a 200 is a tunnel whatever its
	// framing says (Close would read a Content-Length body from it), and
	// fail closes the connection.
	if resp.StatusCode != 200 || br.Buffered() > 0 {
		return fail(fmt.Errorf("proxy tunnel answered %d", resp.StatusCode))
	}
	if !stop() {
		return fail(ctx.Err())
	}
	conn.SetDeadline(time.Time{})
	return conn, nil
}

// proxyEnv is hyper-util's Matcher::from_env, which reqwest (without its
// system-proxy feature) uses: ALL_PROXY, HTTP_PROXY, HTTPS_PROXY, NO_PROXY,
// each upper case first, and no proxy at all under CGI (REQUEST_METHOD).
type proxyEnv struct {
	http, https *proxyTarget
	no          noProxy
}

type proxyTarget struct {
	scheme string
	host   string
	addr   string // host:port
	auth   string // Proxy-Authorization value, or ""
	url    *url.URL
}

func proxyFromEnv(lookupEnv func(string) (string, bool)) *proxyEnv {
	if _, ok := lookupEnv("REQUEST_METHOD"); ok {
		return &proxyEnv{}
	}
	first := func(names ...string) string {
		for _, n := range names {
			if v, ok := lookupEnv(n); ok && utf8.ValidString(v) {
				return v
			}
		}
		return ""
	}
	all := parseProxy(first("ALL_PROXY", "all_proxy"))
	p := &proxyEnv{
		http:  parseProxy(first("HTTP_PROXY", "http_proxy")),
		https: parseProxy(first("HTTPS_PROXY", "https_proxy")),
		no:    parseNoProxy(first("NO_PROXY", "no_proxy")),
	}
	if p.http == nil {
		p.http = all
	}
	if p.https == nil {
		p.https = all
	}
	return p
}

// parseProxy is hyper-util's parse_env_uri: no scheme means http; http,
// https, and the socks schemes are kept (the Rust build has no socks
// support, so a socks proxy fails the connection); anything else is no
// proxy.
func parseProxy(v string) *proxyTarget {
	if v == "" {
		return nil
	}
	if !strings.Contains(v, "://") {
		v = "http://" + v
	}
	u, err := url.Parse(v)
	if err != nil || u.Host == "" {
		return nil
	}
	scheme := strings.ToLower(u.Scheme)
	port := u.Port()
	switch scheme {
	case "http":
		if port == "" {
			port = "80"
		}
	case "https":
		if port == "" {
			port = "443"
		}
	case "socks4", "socks4a", "socks5", "socks5h":
		if port == "" {
			port = "1080"
		}
	default:
		return nil
	}
	t := &proxyTarget{scheme: scheme, host: u.Hostname(), addr: net.JoinHostPort(u.Hostname(), port)}
	if u.User != nil {
		pass, _ := u.User.Password()
		t.auth = "Basic " + base64.StdEncoding.EncodeToString([]byte(u.User.Username()+":"+pass))
	}
	t.url = &url.URL{Scheme: scheme, Host: u.Host, User: u.User}
	return t
}

func (p *proxyEnv) forHost(scheme, host string) *proxyTarget {
	if p == nil || p.no.contains(host) {
		return nil
	}
	if scheme == "https" {
		return p.https
	}
	return p.http
}

// forPlainHTTP is Transport.Proxy, used only for http:// targets. Go
// forwards them through an http proxy; an https or socks proxy for an
// http:// target is not supported here (production endpoints are https).
func (p *proxyEnv) forPlainHTTP(req *http.Request) (*url.URL, error) {
	if req.URL.Scheme != "http" {
		return nil, nil
	}
	t := p.forHost("http", req.URL.Hostname())
	if t == nil || t.scheme != "http" {
		return nil, nil
	}
	return t.url, nil
}

// noProxy is hyper-util's NoProxy: comma-separated entries; IP addresses
// and networks match IP hosts; `*` matches all; any other entry matches the
// domain and its subdomains, ASCII case-insensitive, with or without a
// leading dot. Ports are not compared.
type noProxy struct {
	addrs    []netip.Addr
	prefixes []netip.Prefix
	domains  []string
}

func parseNoProxy(v string) noProxy {
	var n noProxy
	for _, part := range strings.Split(v, ",") {
		part = rustTrim(part)
		if p, err := netip.ParsePrefix(part); err == nil && p.Addr().Zone() == "" {
			n.prefixes = append(n.prefixes, p)
		} else if a, err := netip.ParseAddr(part); err == nil && a.Zone() == "" {
			n.addrs = append(n.addrs, a)
		} else if part != "" {
			n.domains = append(n.domains, part)
		}
	}
	return n
}

func (n noProxy) contains(host string) bool {
	host = strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
	if a, err := netip.ParseAddr(host); err == nil && a.Zone() == "" {
		for _, x := range n.addrs {
			if x == a {
				return true
			}
		}
		for _, p := range n.prefixes {
			if p.Contains(a) {
				return true
			}
		}
		return false
	}
	for _, d := range n.domains {
		switch {
		case asciiEqualFold(d, host) || (strings.HasPrefix(d, ".") && asciiEqualFold(d[1:], host)):
			return true
		case len(host) >= len(d) && asciiEqualFold(host[len(host)-len(d):], d):
			if strings.HasPrefix(d, ".") || (len(host) > len(d) && host[len(host)-len(d)-1] == '.') {
				return true
			}
		case d == "*":
			return true
		}
	}
	return false
}

func asciiEqualFold(a, b string) bool {
	return len(a) == len(b) && asciiLower(a) == asciiLower(b)
}
