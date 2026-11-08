// Smoke test against a running server (AGENTIM_URL, AGENTIM_KEY). Run by CI.
package main

import (
	"bufio"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
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

// wsStatus is the status of a WebSocket handshake to /ws (101: logged in).
func wsStatus(token, device string) int {
	u := must(url.Parse(os.Getenv("AGENTIM_URL")))
	q := url.Values{"token": {token}}
	if device != "" {
		q.Set("device", device)
	}
	c := must(net.Dial("tcp", u.Host))
	defer c.Close()
	fmt.Fprintf(c, "GET /ws?%s HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n", q.Encode(), u.Host)
	line := must(bufio.NewReader(c).ReadString('\n'))
	var code int
	fmt.Sscanf(strings.Fields(line)[1], "%d", &code)
	return code
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
	kid, secret := os.Getenv("AGENTIM_KID"), os.Getenv("AGENTIM_SECRET")
	if wsStatus(must(agentim.CreateUserToken(kid, secret, a, time.Hour)), "smoke-1") != 101 {
		log.Fatal("signed token")
	}
	if wsStatus(must(agentim.CreateUserToken(kid, "wrong", a, time.Hour)), "smoke-1") != 401 {
		log.Fatal("wrong secret")
	}
	must(im.RevokeTokens(ctx, a))
	bot := run + "_bot"
	must(im.UpsertUser(ctx, bot, M{"kind": "agent", "owner": a}))
	k := must(im.IssueAgentKey(ctx, bot, false))
	if !strings.HasPrefix(k["key"].(string), "atk_") || wsStatus(k["key"].(string), "") != 101 {
		log.Fatal("agent key")
	}
	must(im.RevokeAgentKey(ctx, bot, k["device_id"].(string)))
	if len(must(im.ListAgents(ctx, a))["agents"].([]any)) != 1 {
		log.Fatal("list agents")
	}
	if must(im.StopAgent(ctx, bot, "smoke"))["stopped"] != true || must(im.ResumeAgent(ctx, bot))["stopped"] != false {
		log.Fatal("stop and resume")
	}
	if must(im.ExportAgent(ctx, bot, false))["sha256"] == nil {
		log.Fatal("export agent")
	}
	must(im.DestroyAgent(ctx, bot))
	must(im.ListDevices(ctx, a))
	lines := must(im.ExportUser(ctx, a, ""))
	if len(lines) == 0 || lines[0]["type"] == nil {
		log.Fatal("export user")
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
	var err error
	// Files: an image and a file, read back through a short-lived link.
	png, _ := base64.StdEncoding.DecodeString("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
	img := must(im.SendFile(ctx, id, a, "dot.png", png, "", "a dot"))
	doc := must(im.SendFile(ctx, id, a, "notes.txt", []byte("notes"), "text/plain", ""))
	if img["file"].(M)["kind"] != "image" || doc["file"].(M)["kind"] != "file" {
		log.Fatal("file kinds")
	}
	docID := doc["file"].(M)["file_id"].(string)
	link := must(im.FileURL(ctx, id, docID, b, ""))
	r := must(http.Get(os.Getenv("AGENTIM_URL") + link["url"].(string)))
	body, _ := io.ReadAll(r.Body)
	r.Body.Close()
	if string(body) != "notes" {
		log.Fatal("file contents")
	}
	// A small image is its own thumbnail.
	if img["file"].(M)["thumb"] != false || must(im.FileURL(ctx, id, img["file"].(M)["file_id"].(string), b, "thumb"))["variant"] != "original" {
		log.Fatal("thumb")
	}
	_, err = im.FileURL(ctx, id, docID, c, "")
	expect(err, "forbidden")
	_, err = im.SendMessage(ctx, id, M{"as": "nobody", "text": "x"})
	expect(err, "not_found")
	_, err = im.SendMessage(ctx, id, M{"as": c, "text": "x"})
	expect(err, "forbidden")
	must(im.SilenceMember(ctx, id, b, 60, "smoke"))
	_, err = im.SendMessage(ctx, id, M{"as": b, "text": "x"})
	expect(err, "forbidden")
	if must(im.UnsilenceMember(ctx, id, b))["changed"] != true {
		log.Fatal("unsilence")
	}
	must(im.Reports(ctx, "open", 50, 0))
	must(im.DeleteUser(ctx, c))
	must(im.Erasure(ctx, c))
	must(im.RecallMessage(ctx, sent["msg_id"].(string), "system"))
	must(im.DissolveConversation(ctx, id, "system"))
	_, err = im.GetConversation(ctx, id)
	expect(err, "not_found")
	fmt.Println("go sdk smoke ok")
}
