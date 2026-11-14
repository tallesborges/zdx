package miniapp

import (
	"bytes"
	"encoding/json"
	"net/http"
)

// getThreads lists recent top-level threads, or only workers with
// `?kind=workers`.
func (s *server) getThreads(w http.ResponseWriter, r *http.Request, _ call) {
	query, ok := parseQuery(w, r, queryField{name: "kind", kind: queryListKind})
	if !ok {
		return
	}
	kind, ok := query.text("kind")
	if !ok {
		kind = "all"
	}
	body, err := s.engine.ThreadList(r.Context(), kind)
	if err != nil {
		s.viewFailure(w, err, "Thread list", "Thread list failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

// getThread serves one thread projection and sets `delivering` from the
// bot's delivery tracker, sampled around the engine read: a read that sees a
// pending delivery record without its outcome always reports delivering.
func (s *server) getThread(w http.ResponseWriter, r *http.Request, c call) {
	query, ok := parseQuery(w, r, queryField{name: "after", kind: queryU64})
	if !ok {
		return
	}
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return
	}
	var after *uint64
	if n, ok := query.number("after"); ok {
		after = &n
	}

	var finishedBefore uint64
	if s.deliveries != nil {
		finishedBefore = s.deliveries.Finished()
	}
	view, err := s.engine.ThreadView(r.Context(), c.id, after)
	if err != nil {
		s.viewFailure(w, err, "Thread view", "Thread load failed")
		return
	}
	source := view.SourceID
	if source == "" {
		source = c.id
	}
	delivering := s.deliveries != nil &&
		(s.deliveries.Running(source) || s.deliveries.Finished() != finishedBefore)

	body, err := setFields(view.Body, field{"delivering", delivering})
	if err != nil {
		s.log.Warn("Mini App thread view is not a JSON object", "thread_id", c.id, "error", err)
		writeText(w, http.StatusInternalServerError, "Thread load failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

func (s *server) getTrajectory(w http.ResponseWriter, r *http.Request, c call) {
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return
	}
	body, err := s.engine.ThreadTrajectory(r.Context(), c.id)
	if err != nil {
		s.viewFailure(w, err, "Thread trajectory", "Thread trajectory failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

func (s *server) getWorkers(w http.ResponseWriter, r *http.Request, c call) {
	if !validThreadID(c.id) {
		writeText(w, http.StatusBadRequest, "Invalid thread ID")
		return
	}
	body, err := s.engine.ThreadWorkers(r.Context(), c.id)
	if err != nil {
		s.viewFailure(w, err, "Worker list", "Worker list failed")
		return
	}
	writeRawJSON(w, http.StatusOK, body)
}

type field struct {
	key   string
	value any
}

// setFields sets top-level fields of a JSON object. Members keep their
// order and other values their exact bytes. A field the object has takes
// the place of its first occurrence and later duplicates are dropped, so a
// client never sees two values for one key; a missing field is appended.
func setFields(object json.RawMessage, fields ...field) ([]byte, error) {
	dec := json.NewDecoder(bytes.NewReader(object))
	open, err := dec.Token()
	if err != nil {
		return nil, err
	}
	if open != json.Delim('{') {
		return nil, errNotObject
	}
	replaced := map[string]int{}
	for i, f := range fields {
		replaced[f.key] = i
	}
	written := make([]bool, len(fields))
	var out bytes.Buffer
	out.WriteByte('{')
	first := true
	member := func(key []byte, value []byte) {
		if !first {
			out.WriteByte(',')
		}
		first = false
		out.Write(key)
		out.WriteByte(':')
		out.Write(value)
	}
	for dec.More() {
		keyToken, err := dec.Token()
		if err != nil {
			return nil, err
		}
		key, _ := keyToken.(string)
		var value json.RawMessage
		if err := dec.Decode(&value); err != nil {
			return nil, err
		}
		encodedKey, err := marshalJSON(key)
		if err != nil {
			return nil, err
		}
		if i, ok := replaced[key]; ok {
			if written[i] {
				continue
			}
			written[i] = true
			if value, err = marshalJSON(fields[i].value); err != nil {
				return nil, err
			}
		}
		member(encodedKey, value)
	}
	if _, err := dec.Token(); err != nil {
		return nil, err
	}
	if dec.More() {
		return nil, errNotObject
	}
	for i, f := range fields {
		if written[i] {
			continue
		}
		key, err := marshalJSON(f.key)
		if err != nil {
			return nil, err
		}
		value, err := marshalJSON(f.value)
		if err != nil {
			return nil, err
		}
		member(key, value)
	}
	out.WriteByte('}')
	return out.Bytes(), nil
}

// marshalJSON encodes without HTML escaping.
func marshalJSON(value any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(value); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

type notObjectError struct{}

func (notObjectError) Error() string { return "not a JSON object" }

var errNotObject error = notObjectError{}
