// Records the golden answers of the Rust harness (../src/main.rs) for the
// Mini App Go tests. The harness copies the extractors, query structs,
// layers and route table of crates/zdx-bot/src/server.rs and echoes what it
// parsed; it pins the workspace's axum and tower-http, and takes serde_json
// and serde_urlencoded from the workspace lock. To re-record after a change
// there or to the case files, from this directory:
//
//	h=$(mktemp -d) && cp -R .. "$h/harness" && cp ../../../../../../../Cargo.lock "$h/harness/"
//	CARGO_TARGET_DIR="$h/target" cargo build --offline --manifest-path "$h/harness/Cargo.toml"
//	go run . "$h/target/debug/axum-golden" ../..
//
// The last argument is the testdata directory holding the case files.
package main

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"unicode/utf8"
)

type repeat struct {
	Prefix     string `json:"prefix"`
	Unit       string `json:"unit"`
	Count      int    `json:"count"`
	Suffix     string `json:"suffix"`
	SuffixUnit string `json:"suffix_unit"`
}

type tcase struct {
	Name        string  `json:"name"`
	Method      string  `json:"method"`
	Path        string  `json:"path"`
	ContentType string  `json:"content_type"`
	Body        string  `json:"body"`
	BodyB64     string  `json:"body_b64"`
	Repeat      *repeat `json:"repeat"`
	NoAuth      bool    `json:"no_auth"`
}

type gcase struct {
	Name   string   `json:"name"`
	Kind   string   `json:"kind"`
	Accept []string `json:"accept"`
}

type answer struct {
	Name            string `json:"name"`
	Status          int    `json:"status"`
	ContentType     string `json:"content_type,omitempty"`
	ContentEncoding string `json:"content_encoding,omitempty"`
	Vary            string `json:"vary,omitempty"`
	Allow           string `json:"allow,omitempty"`
	Body            string `json:"body"`
}

func body(c tcase) []byte {
	switch {
	case c.BodyB64 != "":
		b, _ := base64.StdEncoding.DecodeString(c.BodyB64)
		return b
	case c.Repeat != nil:
		r := c.Repeat
		var buf bytes.Buffer
		buf.WriteString(r.Prefix)
		buf.WriteString(strings.Repeat(r.Unit, r.Count))
		if r.SuffixUnit != "" {
			buf.WriteString(strings.Repeat(r.SuffixUnit, r.Count))
		}
		buf.WriteString(r.Suffix)
		return buf.Bytes()
	}
	return []byte(c.Body)
}

func send(base, method, path string, headers http.Header, payload []byte) answer {
	if method == "" {
		method = http.MethodGet
	}
	req, err := http.NewRequest(method, base, bytes.NewReader(payload))
	if err != nil {
		panic(err)
	}
	raw, query, _ := strings.Cut(path, "?")
	req.URL.Opaque = raw
	req.URL.RawQuery = query
	if strings.Contains(path, "?") && query == "" {
		req.URL.ForceQuery = true
	}
	req.Header = headers
	tr := &http.Transport{DisableCompression: true}
	res, err := (&http.Client{Transport: tr}).Do(req)
	if err != nil {
		panic(err)
	}
	defer res.Body.Close()
	data, _ := io.ReadAll(res.Body)
	if res.Header.Get("Content-Encoding") == "gzip" {
		zr, err := gzip.NewReader(bytes.NewReader(data))
		if err != nil {
			panic(err)
		}
		data, _ = io.ReadAll(zr)
	}
	text := string(data)
	if !utf8.Valid(data) {
		text = "base64:" + base64.StdEncoding.EncodeToString(data)
	}
	return answer{
		Status:          res.StatusCode,
		ContentType:     res.Header.Get("Content-Type"),
		ContentEncoding: res.Header.Get("Content-Encoding"),
		Vary:            strings.Join(res.Header.Values("Vary"), ", "),
		Allow:           res.Header.Get("Allow"),
		Body:            text,
	}
}

func main() {
	dir := os.Args[2]
	cmd := exec.Command(os.Args[1])
	out, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		panic(err)
	}
	defer cmd.Process.Kill()
	addr, _ := bufio.NewReader(out).ReadString('\n')
	base := "http://" + strings.TrimSpace(addr)

	var cases []tcase
	data, _ := os.ReadFile(dir + "/axum_cases.json")
	if err := json.Unmarshal(data, &cases); err != nil {
		panic(err)
	}
	var answers []answer
	for _, c := range cases {
		h := http.Header{}
		if !c.NoAuth {
			h.Set("Authorization", "tma ok")
		}
		if c.ContentType != "" {
			h.Set("Content-Type", c.ContentType)
		}
		a := send(base, c.Method, c.Path, h, body(c))
		a.Name = c.Name
		answers = append(answers, a)
	}
	write(dir+"/axum_golden.json", answers)

	var gcases []gcase
	data, _ = os.ReadFile(dir + "/axum_gzip_cases.json")
	if err := json.Unmarshal(data, &gcases); err != nil {
		panic(err)
	}
	answers = nil
	for _, c := range gcases {
		h := http.Header{}
		for _, v := range c.Accept {
			h.Add("Accept-Encoding", v)
		}
		path := "/z/" + c.Kind
		a := send(base, "GET", path, h, nil)
		a.Name = c.Name
		a.Body = fmt.Sprintf("%d bytes", len(a.Body))
		answers = append(answers, a)
	}
	write(dir+"/axum_gzip_golden.json", answers)

	var jcases []tcase
	data, _ = os.ReadFile(dir + "/axum_json_cases.json")
	if err := json.Unmarshal(data, &jcases); err != nil {
		panic(err)
	}
	answers = nil
	for _, c := range jcases {
		h := http.Header{}
		h.Set("Authorization", "tma ok")
		h.Set("Content-Type", "application/json")
		a := send(base, "POST", "/api/typed", h, body(c))
		a.Name = c.Name
		answers = append(answers, a)
	}
	write(dir+"/axum_json_golden.json", answers)
}

func write(path string, v any) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		panic(err)
	}
	if err := os.WriteFile(path, buf.Bytes(), 0o644); err != nil {
		panic(err)
	}
}
