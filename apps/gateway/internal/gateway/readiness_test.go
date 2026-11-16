package gateway

import (
	"context"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// serve refuses to start while Missing is not empty, so every dependency
// the production wiring leaves unbuilt must be named there, and an entry
// must go once its dependency is wired. This walks the wiring serve uses.
func TestMissingNamesEveryUnwiredDependency(t *testing.T) {
	ctx := context.Background()
	d := Deps{
		Telegram: telegram.NewClient("1:T", telegram.Options{BaseURL: "http://127.0.0.1:1", Logger: quiet}),
		Engine:   engine.NewClient("/nonexistent/engine.sock"),
		Queues:   bot.NewTopicQueues(ctx), Home: t.TempDir(), Zdx: "zdx", Log: quiet,
	}
	d.Work, d.Admission, d.RequestExit, d.Idle = ctx, &bot.Admission{}, func() {}, func() bool { return true }
	settings := Settings{BotToken: "1:T", Root: "/r"}
	comps := serveComponents(d, settings, ServeOptions{Home: d.Home, Zdx: "zdx"}, quiet)

	missing := strings.Join(Missing, "\n")
	gap := func(key string, open bool) {
		t.Helper()
		if named := strings.Contains(missing, key); named != open {
			if open {
				t.Errorf("%s is not wired, but Missing does not name it", key)
			} else {
				t.Errorf("%s is wired; drop it from Missing", key)
			}
		}
	}

	h := comps.Messages.(*bot.Handler)
	hook, ok := h.Commands.(*commands.Hook)
	if !ok {
		t.Fatalf("commands hook %T", h.Commands)
	}
	v := reflect.ValueOf(hook.Handler).Elem()
	for i := range v.NumField() {
		if f := v.Type().Field(i); f.Type.Kind() == reflect.Interface {
			gap("("+f.Name+")", v.Field(i).IsNil())
		}
	}

	// Every handle of the production bot is wired: the components, the
	// intake handler, the presenter and its deliverer. A nil one is a gap
	// Missing must name as "(<Type>.<Field>)".
	wired := func(name string, v reflect.Value, optional ...string) {
		t.Helper()
		for i := range v.NumField() {
			f := v.Type().Field(i)
			if !f.IsExported() {
				continue
			}
			switch f.Type.Kind() {
			case reflect.Interface, reflect.Func, reflect.Pointer, reflect.Map, reflect.Chan:
				if !slices.Contains(optional, f.Name) {
					gap("("+name+"."+f.Name+")", v.Field(i).IsNil())
				}
			}
		}
	}
	wired("Components", reflect.ValueOf(comps))
	wired("Handler", reflect.ValueOf(h).Elem())
	pres := comps.Renderer.(*bot.Presenter)
	wired("Presenter", reflect.ValueOf(pres).Elem())
	wired("Deliverer", reflect.ValueOf(pres.Deliverer).Elem())

	_, checkOnly := pres.Deliverer.Photos.(bot.CheckOnlyPhotos)
	gap("prepare_photo", checkOnly)
	gap("touched_workers", !hasJSONField(reflect.TypeFor[engine.Event](), "touched_workers"))
}

func hasJSONField(t reflect.Type, name string) bool {
	for i := range t.NumField() {
		if tag, _, _ := strings.Cut(t.Field(i).Tag.Get("json"), ","); tag == name {
			return true
		}
	}
	return false
}
