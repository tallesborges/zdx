package intake

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Tests mirror crates/zdx-bot/src/ingest/mod.rs tests with synthetic fakes:
// no network, token, ZDX_HOME, provider, or Rust service.

const fakeToken = "123456:SYNTHETIC-TOKEN"

var videoBytes = []byte("synthetic-mp4-bytes")

// fakeFiles is a scripted Bot API download transport. files maps a file id
// to its file_path and bytes; any other id is served as videos/<id>.mp4 with
// videoBytes, except: "broken" (getFile refused), "nopath" (no file_path),
// "flaky" (first fetch answers 502), "down" (fetch always 502), "offline"
// (getFile never reaches Telegram).
type fakeFiles struct {
	files map[string]fakeFile

	mu       sync.Mutex
	getFile  []string
	fetches  map[string]int
	maxBytes []int64
}

type fakeFile struct {
	path string
	data []byte
}

func (f *fakeFiles) GetFile(_ context.Context, id string) (*telegram.RemoteFile, error) {
	f.mu.Lock()
	f.getFile = append(f.getFile, id)
	f.mu.Unlock()
	switch id {
	case "broken":
		return nil, &telegram.APIError{Method: "getFile", Code: 400, Description: "Bad Request: file is too big " + fakeToken}
	case "nopath":
		return &telegram.RemoteFile{}, nil
	case "offline":
		return nil, &telegram.TransportError{Method: "getFile", Stage: telegram.NotSent, Detail: "connection refused " + fakeToken}
	}
	path := "videos/" + id + ".mp4"
	if file, ok := f.files[id]; ok {
		path = file.path
	}
	return &telegram.RemoteFile{FilePath: &path}, nil
}

func (f *fakeFiles) DownloadFile(_ context.Context, path string, maxBytes int64) ([]byte, error) {
	f.mu.Lock()
	if f.fetches == nil {
		f.fetches = map[string]int{}
	}
	f.fetches[path]++
	n := f.fetches[path]
	f.maxBytes = append(f.maxBytes, maxBytes)
	f.mu.Unlock()
	if (strings.Contains(path, "flaky") && n == 1) || strings.Contains(path, "down") {
		return nil, &telegram.APIError{Method: "downloadFile", Code: 502, Description: "file download failed with status 502 Bad Gateway"}
	}
	for _, file := range f.files {
		if file.path == path {
			return file.data, nil
		}
	}
	return videoBytes, nil
}

func (f *fakeFiles) getFileCalls() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.getFile)
}

// fakeImages answers inline with the bytes as they are, except for paths in
// saved, which get that saved_only delivery.
type fakeImages struct {
	saved map[string]engine.ImageDelivery
	err   error
	bad   bool
	calls []ImageFile
}

func (p *fakeImages) PrepareImage(_ context.Context, img ImageFile) (PreparedImage, error) {
	p.calls = append(p.calls, img)
	if p.err != nil {
		return PreparedImage{}, p.err
	}
	if p.bad {
		return PreparedImage{Delivery: engine.ImageDelivery{Type: "inline"}}, nil
	}
	for suffix, d := range p.saved {
		if strings.HasSuffix(img.Path, suffix) {
			return PreparedImage{Delivery: d}, nil
		}
	}
	return PreparedImage{
		Delivery: engine.ImageDelivery{Type: "inline"},
		Inline:   &engine.InputImage{MimeType: "image/png", Data: base64.StdEncoding.EncodeToString(img.Data)},
	}, nil
}

type fakeTranscriber struct {
	err   error
	calls []AudioFile
}

func (t *fakeTranscriber) Transcribe(_ context.Context, a AudioFile) (*string, error) {
	t.calls = append(t.calls, a)
	if t.err != nil {
		return nil, t.err
	}
	s := "heard " + a.FileName
	return &s, nil
}

type fixture struct {
	files  *fakeFiles
	images *fakeImages
	audio  *fakeTranscriber
	root   string
	sleeps []time.Duration
	c      *Collector
}

func newFixture(t *testing.T, files map[string]fakeFile) *fixture {
	t.Helper()
	f := &fixture{
		files:  &fakeFiles{files: files},
		images: &fakeImages{},
		audio:  &fakeTranscriber{},
		root:   filepath.Join(t.TempDir(), "telegram"),
	}
	f.c = &Collector{
		Files:       f.files,
		Store:       DirStore{Root: f.root},
		Transcriber: f.audio,
		Images:      f.images,
		Log:         slog.New(slog.NewTextHandler(io.Discard, nil)),
		Sleep: func(ctx context.Context, d time.Duration) error {
			f.sleeps = append(f.sleeps, d)
			return ctx.Err()
		},
	}
	return f
}

// msg decodes a Telegram message in chat 42 from user 7 with extra fields.
func msg(t *testing.T, id int64, fields string) *telegram.Message {
	t.Helper()
	body := fmt.Sprintf(`{"message_id":%d,"chat":{"id":42,"type":"supergroup","is_forum":true},"from":{"id":7,"is_bot":false}`, id)
	if fields != "" {
		body += "," + fields
	}
	var m telegram.Message
	if err := json.Unmarshal([]byte(body+"}"), &m); err != nil {
		t.Fatalf("decode message: %v", err)
	}
	return &m
}

func collect(t *testing.T, f *fixture, head *telegram.Message, grouped ...*telegram.Message) Result {
	t.Helper()
	r, err := f.c.Collect(context.Background(), head, grouped)
	if err != nil {
		t.Fatalf("collect: %v", err)
	}
	return r
}

func savedVideo(t *testing.T, r Result) string {
	t.Helper()
	if len(r.Compose.Videos) != 1 || r.Compose.Videos[0].Status != "saved" || r.Compose.Videos[0].Path == nil {
		t.Fatalf("want one saved video, got %+v", r.Compose.Videos)
	}
	return *r.Compose.Videos[0].Path
}

// Rust: captioned_video_reaches_the_agent_as_a_saved_file.
func TestCaptionedVideoIsSaved(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 10, `"caption":"check the keyboard animation","video":{"file_id":"vid1","file_size":19,"mime_type":"video/mp4"}`))
	path := savedVideo(t, r)
	if want := filepath.Join(f.root, "42", "10_vid1.mp4"); path != want {
		t.Fatalf("path %q, want %q", path, want)
	}
	if data, _ := os.ReadFile(path); string(data) != string(videoBytes) {
		t.Fatalf("saved %q", data)
	}
	if r.Compose.Videos[0].Kind != "Video" || r.Compose.Text == nil || *r.Compose.Text != "check the keyboard animation" {
		t.Fatalf("compose %+v", r.Compose)
	}
}

// Rust: video_note_without_caption_runs_a_turn.
func TestVideoNoteWithoutCaption(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 11, `"video_note":{"file_id":"note1"}`))
	if r.Compose.Text != nil || r.Empty() || r.Compose.Videos[0].Kind != "Video note" {
		t.Fatalf("compose %+v", r.Compose)
	}
	savedVideo(t, r)
}

// Rust: animation_is_loaded_once_despite_its_document_twin.
func TestAnimationLoadsOnceDespiteDocumentTwin(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 12, `"animation":{"file_id":"gif1","file_name":"loop.mp4","mime_type":"video/mp4"},"document":{"file_id":"gif1","file_name":"loop.mp4","mime_type":"video/mp4"}`))
	if len(r.Compose.Documents) != 0 || r.Compose.Videos[0].Kind != "Animation" {
		t.Fatalf("compose %+v", r.Compose)
	}
	if !strings.HasSuffix(savedVideo(t, r), "/12_loop.mp4") || f.files.getFileCalls() != 1 {
		t.Fatalf("getFile calls %d", f.files.getFileCalls())
	}
}

// Rust: video_sent_as_a_file_uses_the_video_path.
func TestVideoDocumentUsesVideoPath(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 13, `"document":{"file_id":"doc1","file_name":"screen.mov","mime_type":"video/quicktime"}`))
	if len(r.Compose.Documents) != 0 || r.Compose.Videos[0].Kind != "Video file" || !strings.HasSuffix(savedVideo(t, r), "/13_screen.mov") {
		t.Fatalf("compose %+v", r.Compose)
	}
}

// Rust: oversized_video_is_reported_without_download.
func TestOversizedVideoReportedWithoutDownload(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 14, fmt.Sprintf(`"video":{"file_id":"big1","file_size":%d}`, MaxVideoBytes+1)))
	v := r.Compose.Videos
	if len(v) != 1 || v[0].Status != "too_large" || v[0].SizeBytes == nil || *v[0].SizeBytes != MaxVideoBytes+1 || v[0].Path != nil {
		t.Fatalf("videos %+v", v)
	}
	if f.files.getFileCalls() != 0 {
		t.Fatal("oversized video was requested")
	}
}

// A video whose size Telegram did not announce is measured after download.
func TestUnannouncedLargeVideoIsMeasured(t *testing.T) {
	big := make([]byte, MaxVideoBytes+1)
	f := newFixture(t, map[string]fakeFile{"v": {"videos/v.mp4", big}})
	r := collect(t, f, msg(t, 15, `"video":{"file_id":"v"}`))
	v := r.Compose.Videos[0]
	if v.Status != "too_large" || *v.SizeBytes != uint64(len(big)) {
		t.Fatalf("video %+v", v)
	}
	if entries, _ := os.ReadDir(filepath.Join(f.root, "42")); len(entries) != 0 {
		t.Fatalf("saved %d files", len(entries))
	}
}

func photoSizes(small, big string) string {
	return fmt.Sprintf(`"photo":[{"file_id":%q,"width":90,"height":60,"file_size":1000},{"file_id":%q,"width":900,"height":600,"file_size":9000}]`, small, big)
}

// Rust: two_photo_album_reaches_one_turn_as_two_images.
func TestTwoPhotoAlbumIsOneTurnWithTwoImages(t *testing.T) {
	f := newFixture(t, map[string]fakeFile{
		"a_big": {"photos/a_big.png", []byte("png-a")},
		"b_big": {"photos/b_big.png", []byte("png-b")},
	})
	head := msg(t, 20, `"media_group_id":"g1","caption":"compare these",`+photoSizes("a_small", "a_big"))
	member := msg(t, 21, `"media_group_id":"g1",`+photoSizes("b_small", "b_big"))
	r := collect(t, f, head, member)

	if got := f.files.getFile; len(got) != 2 || got[0] != "a_big" || got[1] != "b_big" {
		t.Fatalf("getFile %v", got)
	}
	imgs := r.Compose.Images
	if len(imgs) != 2 || !strings.HasSuffix(imgs[0].Path, "/20_a_big.png") || !strings.HasSuffix(imgs[1].Path, "/21_b_big.png") {
		t.Fatalf("images %+v", imgs)
	}
	for _, img := range imgs {
		if img.Delivery.Type != "inline" || img.Delivery.Downscaled {
			t.Fatalf("delivery %+v", img.Delivery)
		}
	}
	want := []string{base64.StdEncoding.EncodeToString([]byte("png-a")), base64.StdEncoding.EncodeToString([]byte("png-b"))}
	if len(r.Images) != 2 || r.Images[0].Data != want[0] || r.Images[1].Data != want[1] {
		t.Fatalf("inline images %+v", r.Images)
	}
	if *r.Compose.Text != "compare these" {
		t.Fatalf("text %q", *r.Compose.Text)
	}
	if f.images.calls[0].Path != imgs[0].Path || string(f.images.calls[0].Data) != "png-a" {
		t.Fatalf("preparer got %+v", f.images.calls[0])
	}
}

// Rust: plain_text_and_captions_are_unchanged.
func TestAlbumTextsAndCaptionsJoin(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 2, `"caption":"  first photo "`), msg(t, 3, `"text":"note"`), msg(t, 4, `"text":"   "`))
	if r.Compose.Text == nil || *r.Compose.Text != "first photo\n\nnote" || r.HadAttachments {
		t.Fatalf("result %+v", r)
	}
}

// Rust: non_inline_images_are_saved_with_guidance (the reason comes from the
// injected preparer, which is Rust's).
func TestSavedOnlyImageIsKeptWithoutInlineCopy(t *testing.T) {
	f := newFixture(t, map[string]fakeFile{"heic": {"documents/photo.heic", []byte("heic")}})
	saved := engine.ImageDelivery{Type: "saved_only", Reason: "the model cannot take image/heif images directly", AskMedia: true}
	f.images.saved = map[string]engine.ImageDelivery{"IMG_1.HEIC": saved}
	r := collect(t, f, msg(t, 23, `"document":{"file_id":"heic","file_name":"IMG_1.HEIC","mime_type":"image/heic"}`))
	if len(r.Compose.Images) != 1 || r.Compose.Images[0].Delivery != saved || len(r.Images) != 0 {
		t.Fatalf("result %+v", r)
	}
	if !strings.HasSuffix(r.Compose.Images[0].Path, "/23_IMG_1.HEIC") {
		t.Fatalf("path %q", r.Compose.Images[0].Path)
	}
}

// Rust: failed_image_downloads_become_notices. Notices never carry the
// transport error.
func TestFailedImageDownloadsBecomeNotices(t *testing.T) {
	f := newFixture(t, nil)
	head := msg(t, 25, `"photo":[{"file_id":"broken","width":900,"height":600}]`)
	member := msg(t, 26, fmt.Sprintf(`"document":{"file_id":"huge","file_name":"big.png","mime_type":"image/png","file_size":%d}`, TelegramDownloadLimitBytes+1))
	r := collect(t, f, head, member)
	if len(r.Compose.Images) != 0 || f.files.getFileCalls() != 1 {
		t.Fatalf("images %d, getFile %d", len(r.Compose.Images), f.files.getFileCalls())
	}
	want := []string{
		"One attachment (photo) could not be loaded from Telegram, so it is missing from this message. Tell the user and ask them to resend it.",
		"An image attachment was not downloaded: it is 20.0 MiB and Telegram bots can only download files up to 20.0 MiB. Ask the user for a smaller file.",
	}
	if strings.Join(r.Compose.Notices, "|") != strings.Join(want, "|") {
		t.Fatalf("notices %q", r.Compose.Notices)
	}
	b, _ := json.Marshal(r.Compose)
	if strings.Contains(string(b), fakeToken) {
		t.Fatal("token reached the compose request")
	}
}

// A getFile answer without file_path and a failed save are notices too.
func TestMissingFilePathAndSaveFailureBecomeNotices(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 27, `"document":{"file_id":"nopath","file_name":"a.txt","mime_type":"text/plain"}`))
	if len(r.Compose.Notices) != 1 || !strings.Contains(r.Compose.Notices[0], "(file)") {
		t.Fatalf("notices %q", r.Compose.Notices)
	}

	f = newFixture(t, nil)
	f.c.Store = DirStore{Root: "relative/telegram"}
	r = collect(t, f, msg(t, 28, `"voice":{"file_id":"v1"}`))
	if len(r.Compose.Audios) != 0 || len(r.Compose.Notices) != 1 || !strings.Contains(r.Compose.Notices[0], "(voice message)") {
		t.Fatalf("result %+v", r)
	}
}

// Rust: a_failed_download_is_tried_again.
func TestFailedDownloadIsTriedAgain(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 30, `"voice":{"file_id":"flaky","mime_type":"audio/ogg"}`))
	if f.files.getFileCalls() != 2 || len(r.Compose.Audios) != 1 || len(r.Compose.Notices) != 0 {
		t.Fatalf("getFile %d, result %+v", f.files.getFileCalls(), r)
	}
	if len(f.sleeps) != 1 || f.sleeps[0] != DownloadRetryDelay {
		t.Fatalf("sleeps %v", f.sleeps)
	}
	a := r.Compose.Audios[0]
	if !strings.HasSuffix(a.Path, "/30_flaky.mp4") || a.Transcript == nil || *a.Transcript != "heard flaky.mp4" {
		t.Fatalf("audio %+v", a)
	}
	if call := f.audio.calls[0]; call.MimeType == nil || *call.MimeType != "audio/ogg" || call.Path != a.Path {
		t.Fatalf("transcriber got %+v", call)
	}
	for _, n := range f.files.maxBytes {
		if n != TelegramDownloadLimitBytes {
			t.Fatalf("download cap %d", n)
		}
	}
}

// Rust: a_download_that_keeps_failing_holds_the_message.
func TestDownloadThatKeepsFailingHoldsTheMessage(t *testing.T) {
	for name, id := range map[string]string{"server error": "down", "transport": "offline"} {
		t.Run(name, func(t *testing.T) {
			f := newFixture(t, nil)
			_, err := f.c.Collect(context.Background(), msg(t, 31, `"caption":"listen to this","voice":{"file_id":"`+id+`"}`), nil)
			var unavailable *DownloadUnavailable
			if !errors.As(err, &unavailable) || unavailable.Label != "voice message" {
				t.Fatalf("err %v", err)
			}
			if f.files.getFileCalls() != DownloadAttempts {
				t.Fatalf("getFile %d", f.files.getFileCalls())
			}
			if len(f.sleeps) != 2 || f.sleeps[0] != DownloadRetryDelay || f.sleeps[1] != 2*DownloadRetryDelay {
				t.Fatalf("sleeps %v", f.sleeps)
			}
			if strings.Contains(err.Error(), fakeToken) || err.Error() != "voice message could not be downloaded from Telegram" {
				t.Fatalf("error text %q", err)
			}
		})
	}
}

// A refusal is not retried; a cancel during the retry wait stops the
// message without a notice or DownloadUnavailable.
func TestRefusalNotRetriedAndCancelStops(t *testing.T) {
	f := newFixture(t, nil)
	collect(t, f, msg(t, 32, `"voice":{"file_id":"broken"}`))
	if f.files.getFileCalls() != 1 || len(f.sleeps) != 0 {
		t.Fatalf("getFile %d, sleeps %v", f.files.getFileCalls(), f.sleeps)
	}

	f = newFixture(t, nil)
	ctx, cancel := context.WithCancel(context.Background())
	f.c.Sleep = func(context.Context, time.Duration) error { cancel(); return context.Canceled }
	_, err := f.c.Collect(ctx, msg(t, 33, `"voice":{"file_id":"down"}`), nil)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err %v", err)
	}
}

// Transcription failure keeps the audio without a transcript; a cancelled
// transcription stops the message.
func TestTranscriptionFailureAndCancel(t *testing.T) {
	f := newFixture(t, nil)
	f.audio.err = errors.New("provider failed")
	r := collect(t, f, msg(t, 34, `"audio":{"file_id":"song","file_name":"talk.m4a","mime_type":"audio/mp4"}`))
	if len(r.Compose.Audios) != 1 || r.Compose.Audios[0].Transcript != nil || !strings.HasSuffix(r.Compose.Audios[0].Path, "/34_talk.m4a") {
		t.Fatalf("audios %+v", r.Compose.Audios)
	}

	f = newFixture(t, nil)
	f.audio.err = fmt.Errorf("transcribe: %w", context.Canceled)
	if _, err := f.c.Collect(context.Background(), msg(t, 35, `"voice":{"file_id":"v"}`), nil); !errors.Is(err, context.Canceled) {
		t.Fatalf("err %v", err)
	}
}

// Documents route by MIME type: image/, audio/, else a saved file.
func TestDocumentsRouteByMimeType(t *testing.T) {
	f := newFixture(t, map[string]fakeFile{
		"pic":  {"documents/file_1.png", []byte("png")},
		"rec":  {"documents/file_2.ogg", []byte("ogg")},
		"pdf":  {"documents/file_3.pdf", []byte("%PDF")},
		"anon": {"documents/file_4", []byte("x")},
	})
	r := collect(t, f,
		msg(t, 40, `"document":{"file_id":"pic","file_name":"shot.png","mime_type":"image/png"}`),
		msg(t, 41, `"document":{"file_id":"rec","mime_type":"audio/ogg"}`),
		msg(t, 42, `"document":{"file_id":"pdf","file_name":"../../report.pdf","mime_type":"application/pdf"}`),
		msg(t, 43, `"document":{"file_id":"anon","file_name":"..","mime_type":"application/octet-stream"}`),
	)
	if len(r.Compose.Images) != 1 || !strings.HasSuffix(r.Compose.Images[0].Path, "/40_shot.png") {
		t.Fatalf("images %+v", r.Compose.Images)
	}
	if len(r.Compose.Audios) != 1 || !strings.HasSuffix(r.Compose.Audios[0].Path, "/41_file_2.ogg") {
		t.Fatalf("audios %+v", r.Compose.Audios)
	}
	docs := r.Compose.Documents
	if len(docs) != 2 || docs[0].FileName != "report.pdf" || docs[0].Path != filepath.Join(f.root, "42", "42_report.pdf") || docs[1].FileName != "file_4" {
		t.Fatalf("documents %+v", docs)
	}
}

// Oversized documents and audio are skipped silently, as in Rust; a
// message left with nothing is Empty with HadAttachments. A document without
// a MIME type is not looked at at all (Rust's collect_attachments).
func TestSkippedAttachmentsLeaveAnEmptyResult(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f,
		msg(t, 50, fmt.Sprintf(`"document":{"file_id":"d","mime_type":"application/zip","file_size":%d}`, MaxDocumentBytes+1)),
		msg(t, 51, fmt.Sprintf(`"voice":{"file_id":"v","file_size":%d}`, MaxAudioBytes+1)),
	)
	if !r.Empty() || !r.HadAttachments || f.files.getFileCalls() != 0 {
		t.Fatalf("result %+v, getFile %d", r, f.files.getFileCalls())
	}

	r = collect(t, f, msg(t, 52, `"document":{"file_id":"d"}`))
	if !r.Empty() || r.HadAttachments {
		t.Fatalf("result %+v", r)
	}
	r = collect(t, f, msg(t, 53, `"photo":[]`))
	if !r.Empty() || !r.HadAttachments {
		t.Fatalf("result %+v", r)
	}
}

// The preparer is Rust's: its failure or an inconsistent answer stops the
// message rather than dropping the image.
func TestImagePreparationFailureStops(t *testing.T) {
	f := newFixture(t, nil)
	f.images.err = errors.New("service unavailable")
	if _, err := f.c.Collect(context.Background(), msg(t, 60, `"photo":[{"file_id":"p","width":1,"height":1}]`), nil); err == nil || !strings.Contains(err.Error(), "prepare image") {
		t.Fatalf("err %v", err)
	}
	f = newFixture(t, nil)
	f.images.bad = true
	if _, err := f.c.Collect(context.Background(), msg(t, 61, `"photo":[{"file_id":"p","width":1,"height":1}]`), nil); err == nil || !strings.Contains(err.Error(), "inconsistent") {
		t.Fatalf("err %v", err)
	}
}

func TestBestPhotoPrefersSizeThenAreaThenLast(t *testing.T) {
	u := func(n uint64) *uint64 { return &n }
	cases := []struct {
		photos []telegram.PhotoSize
		want   string
	}{
		{[]telegram.PhotoSize{{FileID: "a", FileSize: u(10)}, {FileID: "b", FileSize: u(5), Width: 999, Height: 999}}, "a"},
		{[]telegram.PhotoSize{{FileID: "a", Width: 10, Height: 10}, {FileID: "b", Width: 20, Height: 20}}, "b"},
		{[]telegram.PhotoSize{{FileID: "a", Width: 10, Height: 10}, {FileID: "b", Width: 10, Height: 10}}, "b"},
		{[]telegram.PhotoSize{{FileID: "a", Width: -5, Height: 10}}, "a"},
	}
	for _, c := range cases {
		if got, ok := bestPhoto(c.photos); !ok || got.FileID != c.want {
			t.Fatalf("bestPhoto %+v = %q", c.photos, got.FileID)
		}
	}
	if _, ok := bestPhoto(nil); ok {
		t.Fatal("empty list has a best photo")
	}
}

// The compose request has the wire shape of zdx_engine::core::chat_turn::UserInput.
func TestComposeWireShape(t *testing.T) {
	f := newFixture(t, nil)
	f.images.saved = map[string]engine.ImageDelivery{"_p.mp4": {Type: "saved_only", Reason: "r", AskMedia: true}}
	r := collect(t, f,
		msg(t, 70, `"caption":"hi","photo":[{"file_id":"p","width":1,"height":1}]`),
		msg(t, 71, fmt.Sprintf(`"video":{"file_id":"big","file_size":%d}`, MaxVideoBytes+1)),
	)
	b, err := json.Marshal(r.Compose)
	if err != nil {
		t.Fatal(err)
	}
	want := fmt.Sprintf(`{"text":"hi","images":[{"path":%q,"delivery":{"type":"saved_only","reason":"r","ask_media":true}}],"videos":[{"kind":"Video","status":"too_large","size_bytes":%d}]}`,
		filepath.Join(f.root, "42", "70_p.mp4"), MaxVideoBytes+1)
	if string(b) != want {
		t.Fatalf("got  %s\nwant %s", b, want)
	}
}

// Rust: reads_the_text_of_a_rich_message (same fixture and expected text).
func TestRichMessageText(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 1, `"rich_message":{"blocks":[
		{"type":"paragraph","text":"August:"},
		{"type":"list","items":[
			{"label":"•","blocks":[{"type":"paragraph","text":"rent: 460.00"}]},
			{"label":"•","blocks":[{"type":"paragraph","text":"lessons: 296.00"}]}]}]}`))
	if r.Compose.Text == nil || *r.Compose.Text != "August:\n\n• rent: 460.00\n• lessons: 296.00" {
		t.Fatalf("text %v", r.Compose.Text)
	}
}

// Per member: text wins over rich content, then the caption follows; members
// keep arrival order. Rust: extract_text.
func TestAlbumRichTextAndCaptionOrder(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f,
		msg(t, 2, `"caption":" head caption ","rich_message":{"blocks":[{"type":"paragraph","text":"head rich"}]}`),
		msg(t, 3, `"text":"typed wins","rich_message":{"blocks":[{"type":"paragraph","text":"ignored rich"}]}`),
		msg(t, 4, `"rich_message":{"blocks":[{"type":"paragraph","text":"member rich"}]},"caption":"member caption"`),
	)
	want := "head rich\n\nhead caption\n\ntyped wins\n\nmember rich\n\nmember caption"
	if r.Compose.Text == nil || *r.Compose.Text != want {
		t.Fatalf("text %q, want %q", deref(r.Compose.Text), want)
	}
}

// Unknown block types degrade to their nested text; empty or malformed rich
// content adds nothing, and the caption still counts. Rust:
// a_message_without_any_text_stays_empty.
func TestRichUnknownEmptyAndMalformed(t *testing.T) {
	f := newFixture(t, nil)
	r := collect(t, f, msg(t, 5, `"rich_message":{"blocks":[{"type":"future_block","file_id":"secret-id","url":"https://example.invalid","text":"kept words"}]}`))
	if r.Compose.Text == nil || *r.Compose.Text != "kept words" {
		t.Fatalf("text %q", deref(r.Compose.Text))
	}

	for _, fields := range []string{
		`"rich_message":{"blocks":[]}`,
		`"rich_message":{"blocks":"not a list"}`,
		`"rich_message":{"blocks":[{"type":"paragraph","text":"   "}]}`,
	} {
		r := collect(t, f, msg(t, 6, fields))
		if r.Compose.Text != nil || !r.Empty() || r.HadAttachments {
			t.Fatalf("%s: result %+v", fields, r)
		}
		r = collect(t, f, msg(t, 7, fields+`,"caption":"only caption"`))
		if r.Compose.Text == nil || *r.Compose.Text != "only caption" {
			t.Fatalf("%s: text %q", fields, deref(r.Compose.Text))
		}
	}
}

func deref(s *string) string {
	if s == nil {
		return "<nil>"
	}
	return *s
}
