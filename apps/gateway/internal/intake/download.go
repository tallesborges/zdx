package intake

import (
	"context"
	"errors"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// download fetches a Telegram file, trying again on a network or server
// error. It returns Telegram's file path and the bytes.
func (c *Collector) download(ctx context.Context, fileID string) (string, []byte, error) {
	for attempt := 1; ; attempt++ {
		filePath, data, err := c.downloadOnce(ctx, fileID)
		if err == nil {
			return filePath, data, nil
		}
		if attempt >= DownloadAttempts || !isTransient(err) || ctx.Err() != nil {
			return "", nil, err
		}
		c.log().Warn("Telegram download failed; trying again", "attempt", attempt, "err", err)
		if serr := c.sleep(ctx, DownloadRetryDelay*time.Duration(attempt)); serr != nil {
			return "", nil, serr
		}
	}
}

func (c *Collector) downloadOnce(ctx context.Context, fileID string) (string, []byte, error) {
	f, err := c.Files.GetFile(ctx, fileID)
	if err != nil {
		return "", nil, err
	}
	if f == nil || f.FilePath == nil || *f.FilePath == "" {
		return "", nil, errors.New("Telegram file missing file_path")
	}
	data, err := c.Files.DownloadFile(ctx, *f.FilePath, TelegramDownloadLimitBytes)
	if err != nil {
		return "", nil, err
	}
	if len(data) > TelegramDownloadLimitBytes {
		return "", nil, errors.New("Telegram file is larger than the download limit")
	}
	return *f.FilePath, data, nil
}

func (c *Collector) sleep(ctx context.Context, d time.Duration) error {
	if c.Sleep != nil {
		return c.Sleep(ctx, d)
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

// isTransient reports a failure another try can fix: no readable answer
// from Telegram, or a server error. A refusal such as "file is too big" is
// not one.
func isTransient(err error) bool {
	var transport *telegram.TransportError
	if errors.As(err, &transport) {
		return true
	}
	var api *telegram.APIError
	return errors.As(err, &api) && api.Code >= 500
}

var _ Files = (*telegram.Client)(nil)
