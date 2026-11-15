package ui

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// recorder captures the default microphone to a WAV file with ffmpeg
// (macOS avfoundation). There is no audio code in this program: ffmpeg
// records, and the service transcribes (transcribe_audio).
type recorder struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stderr *bytes.Buffer
	path   string
	dir    string
	start  time.Time
}

// audioInput is ffmpeg's avfoundation input: no video, the default audio
// device.
const audioInput = ":default"

// startRecording starts ffmpeg; ctx ending kills it (program exit).
func startRecording(ctx context.Context) (*recorder, error) {
	ffmpeg, err := exec.LookPath("ffmpeg")
	if err != nil {
		return nil, errors.New("voice input needs ffmpeg on PATH (brew install ffmpeg)")
	}
	dir, err := os.MkdirTemp("", "zdx-voice-")
	if err != nil {
		return nil, err
	}
	path := filepath.Join(dir, "voice_input.wav")
	cmd := exec.CommandContext(ctx, ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-i", audioInput,
		"-ac", "1", "-ar", "16000", "-y", path)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		os.RemoveAll(dir)
		return nil, err
	}
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		os.RemoveAll(dir)
		return nil, fmt.Errorf("start ffmpeg: %w", err)
	}
	return &recorder{cmd: cmd, stdin: stdin, stderr: &stderr, path: path, dir: dir, start: time.Now()}, nil
}

// stop ends the capture cleanly (ffmpeg's `q`) and returns the WAV path.
func (r *recorder) stop() (string, error) {
	_, _ = io.WriteString(r.stdin, "q")
	r.stdin.Close()
	done := make(chan error, 1)
	go func() { done <- r.cmd.Wait() }()
	var err error
	select {
	case err = <-done:
	case <-time.After(5 * time.Second):
		_ = r.cmd.Process.Kill()
		err = <-done
	}
	if info, statErr := os.Stat(r.path); statErr != nil || info.Size() <= 44 {
		msg := strings.TrimSpace(r.stderr.String())
		if msg == "" && err != nil {
			msg = err.Error()
		}
		if msg == "" {
			msg = "no audio was captured"
		}
		return "", fmt.Errorf("recording failed: %s", msg)
	}
	return r.path, nil
}

// discard kills the capture and removes its file.
func (r *recorder) discard() {
	_ = r.cmd.Process.Kill()
	_ = r.cmd.Wait()
	r.cleanup()
}

func (r *recorder) cleanup() { os.RemoveAll(r.dir) }
