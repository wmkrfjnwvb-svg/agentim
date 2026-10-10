// Smoke test against a running server (AGENTIM_URL, AGENTIM_KEY). Run by CI.
package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"os"
	"strings"
	"time"

	"github.com/wmkrfjnwvb-svg/agentim/sdk/go/agentim"
)

type M = agentim.M

func must[T any](v T, err error) T {
	if err != nil {
		log.Fatal(err)
	}
	return v
}

func expect(err error, code string) {
	var e *agentim.Error
	if !errors.As(err, &e) || e.Code != code {
		log.Fatalf("expected %s, got %v", code, err)
	}
}

func main() {
	ctx := context.Background()
	im := agentim.New(os.Getenv("AGENTIM_URL"), os.Getenv("AGENTIM_KEY"))
	run := fmt.Sprintf("go%d", time.Now().UnixMilli())
	a, b, c := run+"_a", run+"_b", run+"_c"

	must(im.UpsertUser(ctx, a, M{"display_name": "A"}))
	must(im.BatchUsers(ctx, []M{{"id": b}, {"id": c}}))
	if must(im.GetUser(ctx, a))["display_name"] != "A" {
		log.Fatal("profile")
	}
	if !strings.HasPrefix(must(im.IssueToken(ctx, a, M{"device_name": "smoke"}))["token"].(string), "atk_") {
		log.Fatal("token")
	}
	conv := must(im.CreateConversation(ctx, M{"as": "system", "owner": a, "kind": "group", "title": "smoke", "members": []string{b}}))
	id := conv["id"].(string)
	sent := must(im.SendMessage(ctx, id, M{"as": a, "text": "hello", "client_msg_id": "m1"}))
	again := must(im.SendMessage(ctx, id, M{"as": a, "text": "hello", "client_msg_id": "m1"}))
	if again["duplicate"] != true || again["msg_id"] != sent["msg_id"] {
		log.Fatal("dedup")
	}
	must(im.SendMessage(ctx, id, M{"as": "system", "text": "notice"}))
	must(im.EditMessage(ctx, sent["msg_id"].(string), M{"as": a, "text": "hello!"}))
	dm := must(im.SendToUser(ctx, b, M{"as": a, "text": "hi"}))
	res := must(im.BatchSend(ctx, M{"as": b, "to": []string{a, "nobody"}, "text": "promo"}))["results"].([]any)
	if res[0].(M)["ok"] != true || res[1].(M)["ok"] != false {
		log.Fatal("batch")
	}
	h := must(im.History(ctx, id, 0, 200))
	found := false
	for _, e := range h["events"].([]any) {
		if m, ok := e.(M)["msg"].(M); ok && m["content"] == "hello!" {
			found = true
		}
	}
	if !found {
		log.Fatal("history")
	}
	must(im.MarkRead(ctx, dm["conv_id"].(string), b, 1))
	_, err := im.SendMessage(ctx, id, M{"as": "nobody", "text": "x"})
	expect(err, "not_found")
	_, err = im.SendMessage(ctx, id, M{"as": c, "text": "x"})
	expect(err, "forbidden")
	must(im.RecallMessage(ctx, sent["msg_id"].(string), "system"))
	must(im.DissolveConversation(ctx, id, "system"))
	_, err = im.GetConversation(ctx, id)
	expect(err, "not_found")
	fmt.Println("go sdk smoke ok")
}
