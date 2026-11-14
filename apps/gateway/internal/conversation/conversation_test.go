package conversation

import "testing"

func TestRoundTripAndEdgeCases(t *testing.T) {
	topic := int64(4242)
	id := ThreadID(-1001234567890, &topic)
	if id != "telegram--1001234567890-topic-4242" {
		t.Fatalf("got %s", id)
	}
	r, ok := Parse(id)
	if !ok || r.Chat != -1001234567890 || r.Topic == nil || *r.Topic != topic {
		t.Fatalf("got %+v", r)
	}
	if r, ok := Parse(ThreadID(7, nil)); !ok || r.Chat != 7 || r.Topic != nil {
		t.Fatalf("got %+v", r)
	}
	for _, bad := range []string{"active", "telegram-", "telegram-x", "telegram-1-topic-x", "other-1"} {
		if _, ok := Parse(bad); ok {
			t.Errorf("%q must not parse", bad)
		}
	}
}
