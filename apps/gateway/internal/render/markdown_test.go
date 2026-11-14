package render

import "testing"

// Cases from crates/zdx-bot/src/telegram/markdown.rs `mod tests`.
func TestToTelegramHTML(t *testing.T) {
	cases := []struct{ name, in, want string }{
		{"inline formatting", "**bold** and *italic* and `code`", "<b>bold</b> and <i>italic</i> and <code>code</code>"},
		{"heading", "## Answer\n\nBody text", "<b>Answer</b>\n\nBody text"},
		{"link href escaped", "[link](https://example.com/?a=1&b=2)", `<a href="https://example.com/?a=1&amp;b=2">link</a>`},
		{"fenced code with language", "```rust\nlet a = 1 < 2;\n```", `<pre><code class="language-rust">let a = 1 &lt; 2;</code></pre>`},
		{"fenced code without language", "```\njust code\n```", "<pre>just code</pre>"},
		{"blank lines in code", "```\na\n\nb\n```", "<pre>a\n\nb</pre>"},
		{"bullet list", "Intro\n\n- one\n- two\n\nOutro", "Intro\n\n- one\n- two\n\nOutro"},
		{"ordered list", "1. one\n2. two", "1. one\n2. two"},
		{"nested list", "- one\n  - nested\n- two", "- one\n  - nested\n- two"},
		{"blockquote", "> quoted line\n\nafter", "<blockquote>quoted line</blockquote>\n\nafter"},
		{"nested blockquotes flatten", "> outer\n>\n> > inner", "<blockquote>outer\n\ninner</blockquote>"},
		{"code block suspends blockquote", "> ```\n> code\n> ```", "<pre>code</pre>"},
		{"inline code plain in blockquote", "> before `code` after", "<blockquote>before code after</blockquote>"},
		{"blockquote formatting continuous", "> **before `code` after**", "<blockquote><b>before code after</b></blockquote>"},
		{"link plain in blockquote", "> before [link](https://example.com) after", "<blockquote>before link (https://example.com) after</blockquote>"},
		{"many code spans in blockquote",
			"> `account.createAccountProof` takes `productAccountId` with `dotNsIdentifier`.",
			"<blockquote>account.createAccountProof takes productAccountId with dotNsIdentifier.</blockquote>"},
		{"bold reopened around code", "**bold with `code` inside**", "<b>bold with </b><code>code</code><b> inside</b>"},
		{"link reopened around code", "[run `zdx` now](https://example.com)",
			`<a href="https://example.com">run </a><code>zdx</code><a href="https://example.com"> now</a>`},
		{"stray brackets", "very old (<Android 8) devices & more", "very old (&lt;Android 8) devices &amp; more"},
		{"generic raw html", "Vec<T>", "Vec&lt;T&gt;"},
		{"unknown tags", "<task>do this</task>", "&lt;task&gt;do this&lt;/task&gt;"},
		{"entity in code span once", "`a &amp; b`", "<code>a &amp;amp; b</code>"},
		{"table", "| a | b |\n| - | - |\n| 1 | 2 |", "a | b\n1 | 2"},
		{"image alt only", "![alt text](img.png)", "alt text"},
		{"legacy html passes",
			"<b>Answer:</b> Use <code>git rebase -i HEAD~3</code>.\n\n- Pick the commits to squash\n- Save and close the editor\n",
			"<b>Answer:</b> Use <code>git rebase -i HEAD~3</code>.\n\n- Pick the commits to squash\n- Save and close the editor"},
		{"inline legacy html", "Details here <i>Full details attached ↓</i>", "Details here <i>Full details attached ↓</i>"},
		{"legacy keeps markers in code", "<code>*literal*</code>", "<code>*literal*</code>"},
		{"unclosed legacy escaped", "<b>open", "&lt;b&gt;open"},
		{"mixed legacy escaped", "<b>legacy</b><task>x</task>", "&lt;b&gt;legacy&lt;/b&gt;&lt;task&gt;x&lt;/task&gt;"},
		{"forbidden nesting escaped", "<blockquote><code>x</code></blockquote>", "&lt;blockquote&gt;&lt;code&gt;x&lt;/code&gt;&lt;/blockquote&gt;"},
		{"a without href", "<a>link</a>", "&lt;a&gt;link&lt;/a&gt;"},
		{"span without class", "<span>secret</span>", "&lt;span&gt;secret&lt;/span&gt;"},
		{"unknown attribute", "<b arbitrary>bold</b>", "&lt;b arbitrary&gt;bold&lt;/b&gt;"},
		{"extra blank lines", "a\n\n\n\nb", "a\n\nb"},
		{"soft break", "one\ntwo", "one two"},
	}
	for _, c := range cases {
		if got := ToTelegramHTML(c.in); got != c.want {
			t.Errorf("%s: ToTelegramHTML(%q)\n got %q\nwant %q", c.name, c.in, got, c.want)
		}
	}
}

func TestToTelegramHTMLCanonicalLegacyAttributes(t *testing.T) {
	for _, in := range []string{
		`<a href="https://example.com">link</a>`,
		`<span class="tg-spoiler">secret</span>`,
		`<blockquote expandable>quote</blockquote>`,
		`<tg-emoji emoji-id="123">🙂</tg-emoji>`,
		`<pre><code class="language-rust">fn main() {}</code></pre>`,
	} {
		if got := ToTelegramHTML(in); got != in {
			t.Errorf("ToTelegramHTML(%q) = %q", in, got)
		}
	}
}

func TestToTelegramHTMLIsIdempotent(t *testing.T) {
	once := ToTelegramHTML("**bold** `code`\n\n- item\n\n> quote\n\n```rust\nfn main() {}\n```")
	if twice := ToTelegramHTML(once); twice != once {
		t.Fatalf("once %q\ntwice %q", once, twice)
	}
}

func TestRustGoldenMarkdown(t *testing.T) {
	for _, c := range loadGolden(t).Markdown {
		if got := ToTelegramHTML(c.In); got != c.Out {
			t.Errorf("ToTelegramHTML(%q)\n got %q\nwant %q", c.In, got, c.Out)
		}
	}
}
