package telegram

import (
	"encoding/json"
	"testing"
)

// Cases ported from crates/zdx-bot/src/telegram/rich.rs and ingest/mod.rs.
func TestRichTextMatchesTheRustBot(t *testing.T) {
	cases := map[string]struct{ blocks, want string }{
		"paragraph and list": {
			`[{"type":"paragraph","text":"Agosto:"},{"type":"list","items":[
				{"label":"•","blocks":[{"type":"paragraph","text":"Item one: 460,00"}]},
				{"label":"•","blocks":[{"type":"paragraph","text":"Item two: 296,00"}]}]}]`,
			"Agosto:\n\n• Item one: 460,00\n• Item two: 296,00",
		},
		"styled and nested rich text": {
			`[{"type":"paragraph","text":["total ",{"type":"bold","text":[{"type":"italic","text":"1.552,00"}]},{"type":"url","text":"receipt","url":"https://example.com/r"}]}]`,
			"total 1.552,00receipt (https://example.com/r)",
		},
		"headings code quotes and tasks": {
			`[{"type":"heading","text":"Report","size":2},{"type":"pre","text":"cargo test","language":"bash"},
			{"type":"blockquote","blocks":[{"type":"paragraph","text":"quoted"}],"credit":"author"},
			{"type":"list","items":[{"has_checkbox":true,"is_checked":true,"blocks":[{"type":"paragraph","text":"done"}]},
				{"has_checkbox":true,"blocks":[{"type":"paragraph","text":"todo"}]}]},{"type":"divider"}]`,
			"## Report\n\n```bash\ncargo test\n```\n\n> quoted\n> — author\n\n- [x] done\n- [ ] todo\n\n---",
		},
		"table with header row": {
			`[{"type":"table","cells":[[{"text":"Month","is_header":true},{"text":"Total","is_header":true}],[{"text":"August"},{"text":"1.552,00"}]],"caption":"Expenses"}]`,
			"| Month | Total |\n| --- | --- |\n| August | 1.552,00 |\nExpenses",
		},
		"media blocks are named": {
			`[{"type":"photo","photo":[{"file_id":"abc"}],"caption":{"text":"the receipt"}},{"type":"voice_note","voice_note":{"file_id":"def"}}]`,
			"[photo] the receipt\n\n[voice note]",
		},
		"unknown block and text types keep their text": {
			`[{"type":"future_block","text":"still readable","blocks":[{"type":"paragraph","text":"nested too"}]},{"type":"paragraph","text":{"type":"future_style","text":"styled"}}]`,
			"still readable\nnested too\n\nstyled",
		},
		"unknown block never leaks ids or urls": {
			`[{"type":"future_media","file_id":"SECRET_ID","url":"https://x/SECRET","label":"shown"}]`,
			"shown",
		},
		"heading size clamps; a non-integer size is 1": {
			`[{"type":"heading","text":"a","size":9},{"type":"heading","text":"b","size":2.5}]`,
			"###### a\n\n# b",
		},
		"math, emoji, anchors": {
			`[{"type":"paragraph","text":[{"type":"mathematical_expression","expression":"x^2"}," ",{"type":"custom_emoji","alternative_text":"🙂"},{"type":"anchor","name":"n"}]}]`,
			"x^2 🙂",
		},
		"link whose text is the url": {
			`[{"type":"paragraph","text":{"type":"url","text":"https://e.com","url":"https://e.com"}}]`,
			"https://e.com",
		},
		"rust bot message case": {
			`[{"type":"paragraph","text":"August:"},{"type":"list","items":[{"label":"•","blocks":[{"type":"paragraph","text":"rent: 460.00"}]},{"label":"•","blocks":[{"type":"paragraph","text":"lessons: 296.00"}]}]}]`,
			"August:\n\n• rent: 460.00\n• lessons: 296.00",
		},
	}
	for name, c := range cases {
		got := RichText(json.RawMessage(`{"blocks":` + c.blocks + `}`))
		if got == nil || *got != c.want {
			t.Errorf("%s:\n got %q\nwant %q", name, deref(got), c.want)
		}
	}
	for _, empty := range []string{`{"blocks":[]}`, `{"blocks":[{"type":"anchor","name":"top"}]}`, `not json`, ``} {
		if got := RichText(json.RawMessage(empty)); got != nil {
			t.Errorf("%q must have no text, got %q", empty, *got)
		}
	}
}

func TestPlainTextPrefersText(t *testing.T) {
	text := "typed"
	m := &Message{Text: &text, RichMessage: json.RawMessage(`{"blocks":[{"type":"paragraph","text":"rich"}]}`)}
	if *m.PlainText() != "typed" {
		t.Fatal("text wins")
	}
	m.Text = nil
	if *m.PlainText() != "rich" {
		t.Fatal("rich text when text is unset")
	}
}

func deref(s *string) string {
	if s == nil {
		return "<nil>"
	}
	return *s
}
