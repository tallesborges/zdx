---
name: html-artifacts
description: Build one HTML page that helps the user understand, decide, or try something, read and annotated on phone and desktop. The html-artifact-specialist subagent loads this skill automatically; other agents delegate this work to it. Also use it when a brief names this skill. Typical requests are "make this visual", "make an HTML for this", "help me understand this PR", or "too much text, make it more visual". Not for product UI or code that ships in a repository, images, or PDFs.
---

# HTML artifacts

The user describes a problem: a PR to understand, options to weigh, a workflow to try. HTML pages are how the user explores and thinks through a topic, so the page is often the main result and the chat reply stays short. Your job is to find the visual form that makes the problem clear or lets the user try it, and build it as one HTML page. You choose the form, the structure, and the interaction. The user iterates with annotations.

## What the user values

- Visual over prose. The user learns by seeing and trying, so show it when it can be shown. Choose the form that helps the user see or try the subject. Do not mistake arranging text into cards or tables for a visual explanation. Apply visual explanation across the page, not only to its main topic. Do not leave the remaining substantive points as a report below one strong visual. Use text where it explains more clearly, rather than forcing a visual.
- A small reading load by default. The page as it loads (everything visible without a tap, including headings, labels, diagram text, and text drawn by scripts) holds at most about 600 words. That default view gives the whole picture: the answer, each part the user asked about, and every qualification that changes the answer. Views, states, and details the user opens to go deeper are not counted, but each stays short. The limit is on reading, not on figures or behavior. Do not meet it by shrinking type, by moving prose into diagram labels, or by hiding the answer itself. A page that answers a question leads with the answer. A page to operate opens in a working state.
- Plain language, per the global Writing Style rule, in the user's language.
- A page that works well on a phone and on a desktop.
- The user's explicit requests win over anything here. When they ask for a format, a library, a layout, or Mermaid, use it.

## Stay true to the sources

- Read the sources the task names (threads, files, diffs, notes) instead of working from a summary. Read the thread the task came from when context is missing.
- Show what the sources support. Keep what is measured, inferred, and assumed distinguishable, and do not drop important facts to make the page shorter. A fact one tap away is kept, not dropped.
- Use the user's own text verbatim when you show it.

## Judgment that tends to work

These are observations, not rules. Choose what fits the problem.

- A visual earns its place when it shows a space, a structure, a flow, a quantity, or something to try. When a sentence is faster, write the sentence.
- Let what the user wants to do with the page pick its form and interaction, not the topic. A page can hold the right information and still miss if it does not support the way they asked to explore it. The same PR can need the new request path for "how does this work now" and a before/after for "what did this change".
- Draw the mechanism, not its name. For options, draw what differs between them.
- An answer that does not depend on the reader's input is often a diagram. A question about amounts is often a chart or a single number, with a table when exact values matter. "What happens if I do X" or "how does this feel" is often a working prototype. A plan, workflow, or feature that people use through screens is often clearer as a mockup the user steps through than as a diagram or a description of it.
- When the real system has a control, the figure can have it too. Interaction is for exploring the content. Use disclosure to keep detail available. When interaction helps explain the subject, let the user explore its behavior rather than only reveal more text. A control earns its place when each action shows a clear effect, ideally everywhere that effect matters (the result, the code or message it produces, the state it changes). An empty state says what to try next.
- The page is useful when it loads: the main answer is visible, or the tool is already in a realistic working state with sample data. Nothing important appears only on hover or after an animation.
- A prototype proves the core interaction. When the real mechanism is the point and cheap to run in the page (a browser boundary, a parser, a state rule), run it for real. Simulate the supporting parts (data, network, other screens) and say on the page what is simulated. A mockup of an existing product starts from that product's real source and values.
- SVG is the default for diagrams. Use Mermaid when the user asks for it; a pinned Mermaid library from a CDN renders it in the page.

## Where the page runs

This section is the one place these facts live.

- The caller attaches the file in Telegram. The user opens it with **Open artifact** in the ZDX Mini App, or in a normal browser on the phone or the desktop.
- In the Mini App the user marks elements, draws, and adds voice or images. Those notes come back to you as `[artifact feedback]` (see Edits and feedback). The page needs no way to collect feedback.
- The Mini App loads the page from a `blob:` URL in a frame with `sandbox="allow-scripts"` and no same-origin access:
  - Scripts, buttons, inputs, and other script-driven controls work.
  - Storage and cookies are not available (access throws). Keep state in memory, or wrap access in `try`.
  - The sandbox flags block native form submission, popups and new tabs, `alert`/`confirm` dialogs, and downloads. Drive controls with click or input handlers. (This follows from the flags; not every case was tested in the viewer.)
  - A plain link navigates the frame away from the page, and annotation stops there.
  - Sibling files do not resolve. Keep local images, styles, and data inside the one file.
  - No content policy blocks remote resources in the current viewer, so a pinned library or font from a CDN can load. A CDN can still fail (network, CORS). Stay self-contained by default and load a library when it does real work.

## Edits and feedback

- For a revision or `[artifact feedback]`, edit the same file and build on the earlier work. Do not start from zero.
- Feedback notes carry an intent (change, keep as is, question, note), page selectors, and a `Bundle:` folder with `feedback.json`, sketches, and images. Read the bundle, change what is asked, keep what is marked "Keep as is", and answer questions in your reply unless the answer belongs in the page.
- When a note is unclear, make a small targeted edit rather than a redesign.
- Stable `id`s on sections help new notes attach to the right place.

## Save and return

- Save to `$ZDX_ARTIFACT_DIR/<short-name>.html`.
- Run a browser or take screenshots only when the user asks, or to diagnose a reported defect.
- Return `HTML: <absolute path>` first, then a short summary of the page and open points. The caller gets your thread id from its tool result.
- Do not send messages. The caller delivers the file.

## Craft notes

Know-how from earlier artifacts. Use what helps; none of it is required.

### Page

- The page reads alone: a short title, the main point or the working tool, and enough context that someone without the conversation understands it.
- One column on a phone; use the extra width on desktop, for example a figure next to its explanation.
- The page should not scroll sideways on a phone. `minmax(0,1fr)`, `min-width: 0`, and `max-width: 100%` help. Wide tables, code, and diagrams can scroll inside their own box.
- `<details>` keeps evidence, method, sources, and exact code close to the figure they support without filling the default view.
- Repeated elements share edges and padding. Card styling marks a separate object; it does not need to wrap everything.
- Touch has no hover, so every value and control should work by tap, with targets large enough for a finger.
- Design the page on purpose. A clear order (the main idea, then the part to look at or use, then detail), a small type scale, and controls that show their state the same way everywhere make a page feel considered.
- Ground the look in the subject. Its real names, identifiers, units, and realistic sample data read better than generic labels. A short, specific page name helps it stand out among many artifacts.
- Let color carry meaning: an accent for state, focus, or the thing that matters, neutrals for the rest. Monospace suits code, identifiers, and raw data.
- Choose a look that fits the task and the product it shows, and keep it legible: readable body size, enough contrast, a legend when color carries meaning. CSS variables keep a theme consistent; if the page supports light and dark mode, check contrast in both.
- Some looks read as generic when they appear without a reason: purple-blue gradients, glow, emoji as bullets, the same card around every block, a large hero that pushes the content down. Use one when the content calls for it.

### SVG diagrams

- `width="100%"` scales the whole SVG, labels included, by (rendered width / viewBox width). A 360 px phone with normal margins renders a figure about 328 px wide, so a 680-wide viewBox turns 14 px labels into about 7 px. Past artifacts often had this problem.
- Labels stay at about 11 px or more when the viewBox width is at most about 28 times the smallest label size. Other options: split a wide diagram into an overview and parts, or give it a horizontal scroll box with a minimum width. On desktop the same scaling makes labels large; cap the figure width or place figures side by side.
- For zoom, per-figure controls that change the SVG width inside its own scroll box work better than scaling the whole page.
- SVG text does not wrap. Size each box from its longest label (about 0.6 times the font size per character for sans text, more for symbols) and break lines with `tspan`. Too many boxes in one row get cramped on a phone; wrap them into rows or split the diagram.
- Connector paths need `fill="none"`, or they render as filled shapes. Lines that stop at box edges, and arrows routed around other boxes, read cleanly. Label an arrow when its meaning is not obvious from its ends. One arrow marker with `stroke="context-stroke"` takes the color of each line that uses it.
- On a transparent SVG, the page background decides text contrast. Set the wrapper background and check contrast in both themes.
- Reference diagrams (boxes, labels, containment) help the reader point at parts. Intuition diagrams (a spatial picture of the mechanism) help the reader feel how it works. A page can use both, intuition first.
- Cycles drawn as rings tend to collide with their own labels; a stepper, or a line with a return arrow, often reads better.
- One SVG per figure, with `role="img"` and a `<title>`, keeps each figure easy to caption and annotate.

### Charts

- Choose the form before the colors. A single number can be a large figure, one ratio a meter, a few exact values a table; a chart earns its place when the shape of the data matters.
- Draw to scale and show units. One y-axis per chart; two axes on one chart invite false comparisons.
- Color the series that matters and keep the rest gray. A color stays with its entity when a filter or view changes.
- Sequential data: one hue from light to dark. Diverging data: two hues with a neutral middle.
- Round displayed numbers to what the reader needs.
- Tooltips can add detail, but every value should be reachable without hover.
- Hand-written SVG suits small charts. A charting library helps when it saves real work (many points, interaction).

### Prototypes and mockups

- A prototype answers a design question. Writing that question at the top helps the user judge it.
- Pick a fidelity (sketch, clickable, or wired to real logic) and state it as an assumption instead of asking the user.
- What to fake includes data, network, and other screens.
- For rules or state machines, a small pure logic module and a view of the full state after each step make behavior easy to check.
- For a mockup of an existing product, copy its exact values (colors, spacing, text, components) and use a clear placeholder for any asset you do not have.
- A phone or desktop frame helps when it matches where the real thing runs.
- Variants help when the request is exploratory. They differ in structure, not only in style, and fit in one file with a switcher.
- A short hand-off at the end (must have, nice to have, cut) helps when the prototype will become real work.
