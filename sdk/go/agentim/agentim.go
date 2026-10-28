// Package agentim is a thin client for the AgentIM open API /v1
// (docs/07-open-api.md). Standard library only.
package agentim

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// Error is an error answered by the server (RFC 9457 problem details). Code
// is one of the protocol's codes: forbidden, not_found, conflict, blocked...
type Error struct {
	Status int
	Code   string
	Detail string
}

func (e *Error) Error() string { return fmt.Sprintf("%d %s: %s", e.Status, e.Code, e.Detail) }

// Client calls the open API with one API key.
type Client struct {
	BaseURL string
	APIKey  string
	Retries int
	HTTP    *http.Client
}

// New returns a client with 3 retries and a 30 second timeout.
func New(baseURL, apiKey string) *Client {
	return &Client{
		BaseURL: strings.TrimRight(baseURL, "/"),
		APIKey:  apiKey,
		Retries: 3,
		HTTP:    &http.Client{Timeout: 30 * time.Second},
	}
}

// M is a JSON object.
type M = map[string]any

func newKey() string {
	b := make([]byte, 16)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

// Do is the low-level call: POSTs get an Idempotency-Key reused across
// retries; GET, PUT and POST are retried on network errors, 429 and 503.
func (c *Client) Do(ctx context.Context, method, path string, body any, out any) error {
	if body == nil {
		return c.send(ctx, method, path, nil, "", out)
	}
	data, err := json.Marshal(clean(body))
	if err != nil {
		return err
	}
	return c.send(ctx, method, path, data, "application/json", out)
}

// DoRaw sends bytes as the body (application/octet-stream), e.g. a file.
func (c *Client) DoRaw(ctx context.Context, method, path string, data []byte, out any) error {
	return c.send(ctx, method, path, data, "application/octet-stream", out)
}

func (c *Client) send(ctx context.Context, method, path string, data []byte, contentType string, out any) error {
	idem := newKey()
	retryable := method == http.MethodGet || method == http.MethodPut || method == http.MethodPost
	for attempt := 0; ; attempt++ {
		req, err := http.NewRequestWithContext(ctx, method, c.BaseURL+path, bytes.NewReader(data))
		if err != nil {
			return err
		}
		req.Header.Set("authorization", "Bearer "+c.APIKey)
		if contentType != "" {
			req.Header.Set("content-type", contentType)
		}
		if method == http.MethodPost {
			req.Header.Set("idempotency-key", idem)
		}
		res, err := c.HTTP.Do(req)
		if err != nil {
			if retryable && attempt < c.Retries {
				sleep(ctx, backoff(attempt))
				continue
			}
			return err
		}
		text, _ := io.ReadAll(res.Body)
		res.Body.Close()
		if res.StatusCode < 300 {
			if out == nil || len(text) == 0 {
				return nil
			}
			return json.Unmarshal(text, out)
		}
		if retryable && attempt < c.Retries && (res.StatusCode == 429 || res.StatusCode == 503) {
			wait := backoff(attempt)
			if s, err := strconv.Atoi(res.Header.Get("retry-after")); err == nil && s > 0 {
				wait = time.Duration(s) * time.Second
			}
			sleep(ctx, wait)
			continue
		}
		var p struct{ Code, Detail string }
		_ = json.Unmarshal(text, &p)
		if p.Code == "" {
			p.Code, p.Detail = "error", string(text)
		}
		return &Error{Status: res.StatusCode, Code: p.Code, Detail: p.Detail}
	}
}

// clean drops nil and empty values of a top-level map so optional fields
// are left out.
func clean(b any) any {
	m, ok := b.(M)
	if !ok {
		return b
	}
	out := M{}
	for k, v := range m {
		switch x := v.(type) {
		case nil:
			continue
		case string:
			if x == "" {
				continue
			}
		}
		out[k] = v
	}
	return out
}

func backoff(attempt int) time.Duration {
	d := 200 * time.Millisecond << attempt
	if d > 5*time.Second {
		d = 5 * time.Second
	}
	return d
}

func sleep(ctx context.Context, d time.Duration) {
	select {
	case <-ctx.Done():
	case <-time.After(d):
	}
}

var q = url.PathEscape

// ---- login

// CreateUserToken signs a client token for one of your users (docs/13
// §4.2): an HS256 JWT with the signing key from `agentim signing-key
// create`. Do this in your backend after your own login and give the token
// to your app; no call to AgentIM is made.
func CreateUserToken(kid, secret, userID string, expiresIn time.Duration) (string, error) {
	if kid == "" || secret == "" {
		return "", fmt.Errorf("kid and secret are required")
	}
	enc := base64.RawURLEncoding
	iat := time.Now().Unix()
	head, err := json.Marshal(M{"alg": "HS256", "typ": "JWT", "kid": kid})
	if err != nil {
		return "", err
	}
	body, err := json.Marshal(M{"sub": userID, "iat": iat, "exp": iat + int64(expiresIn/time.Second)})
	if err != nil {
		return "", err
	}
	input := enc.EncodeToString(head) + "." + enc.EncodeToString(body)
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write([]byte(input))
	return input + "." + enc.EncodeToString(mac.Sum(nil)), nil
}

// RevokeTokens logs a person out everywhere: client tokens issued until
// now are refused.
func (c *Client) RevokeTokens(ctx context.Context, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/users/"+q(userID)+"/tokens", nil, &out)
}

// ---- agent keys

func (c *Client) IssueAgentKey(ctx context.Context, agentID string, revokeOthers bool) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/agents/"+q(agentID)+"/keys", M{"revoke_others": revokeOthers}, &out)
}

func (c *Client) RevokeAgentKeys(ctx context.Context, agentID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/agents/"+q(agentID)+"/keys", nil, &out)
}

func (c *Client) RevokeAgentKey(ctx context.Context, agentID, deviceID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/agents/"+q(agentID)+"/keys/"+q(deviceID), nil, &out)
}

// ---- agents (docs/13 §4.1)

func (c *Client) ListAgents(ctx context.Context, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/users/"+q(userID)+"/agents", nil, &out)
}

func (c *Client) StopAgent(ctx context.Context, agentID, reason string) (M, error) {
	var out M
	var body any
	if reason != "" {
		body = M{"reason": reason}
	}
	return out, c.Do(ctx, "POST", "/v1/agents/"+q(agentID)+":stop", body, &out)
}

func (c *Client) ResumeAgent(ctx context.Context, agentID string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/agents/"+q(agentID)+":resume", nil, &out)
}

func (c *Client) TransferAgent(ctx context.Context, agentID, owner string, rotateKeys bool) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/agents/"+q(agentID)+":transfer", M{"owner": owner, "rotate_keys": rotateKeys}, &out)
}

func (c *Client) DestroyAgent(ctx context.Context, agentID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/agents/"+q(agentID), nil, &out)
}

func (c *Client) ExportAgent(ctx context.Context, agentID string, withConversations bool) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/agents/"+q(agentID)+"/export?with_conversations="+strconv.FormatBool(withConversations), nil, &out)
}

// ImportAgent creates an agent from a package. allowUnverified accepts a
// package this deployment cannot verify (unsigned, or from another deployment).
func (c *Client) ImportAgent(ctx context.Context, pkg M, owner, name string, allowUnverified bool) (M, error) {
	var out M
	body := M{"package": pkg, "owner": owner, "allow_unverified": allowUnverified}
	if name != "" {
		body["name"] = name
	}
	return out, c.Do(ctx, "POST", "/v1/agents:import", body, &out)
}

// ---- devices

func (c *Client) ListDevices(ctx context.Context, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/users/"+q(userID)+"/devices", nil, &out)
}

func (c *Client) LogoutDevice(ctx context.Context, userID, deviceID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/users/"+q(userID)+"/devices/"+q(deviceID), nil, &out)
}

// ---- compliance (docs/04 §4.2)

// DeleteUser deletes a person's account; it returns the erasure's progress.
func (c *Client) DeleteUser(ctx context.Context, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/users/"+q(userID), nil, &out)
}

func (c *Client) Erasure(ctx context.Context, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/users/"+q(userID)+"/erasure", nil, &out)
}

// ExportUser returns the person's own data, one object per line (NDJSON);
// after continues from a line's cursor.
func (c *Client) ExportUser(ctx context.Context, userID, after string) ([]M, error) {
	path := "/v1/users/" + q(userID) + "/export"
	if after != "" {
		path += "?after=" + url.QueryEscape(after)
	}
	req, err := http.NewRequestWithContext(ctx, "GET", c.BaseURL+path, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+c.APIKey)
	res, err := c.HTTP.Do(req)
	if err != nil {
		return nil, err
	}
	defer res.Body.Close()
	data, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, err
	}
	if res.StatusCode >= 300 {
		var p struct{ Code, Detail string }
		_ = json.Unmarshal(data, &p)
		return nil, &Error{Status: res.StatusCode, Code: p.Code, Detail: p.Detail}
	}
	var lines []M
	for _, l := range strings.Split(string(data), "\n") {
		if l == "" {
			continue
		}
		var m M
		if err := json.Unmarshal([]byte(l), &m); err != nil {
			return nil, err
		}
		lines = append(lines, m)
	}
	return lines, nil
}

// ---- moderation

func (c *Client) Reports(ctx context.Context, state string, limit, offset int) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", fmt.Sprintf("/v1/reports?state=%s&limit=%d&offset=%d", url.QueryEscape(state), limit, offset), nil, &out)
}

func (c *Client) GetReport(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/reports/"+q(id), nil, &out)
}

// ResolveReport takes action (none, remove_content, suspend_user, ban_user),
// reason and, for suspend_user, days.
func (c *Client) ResolveReport(ctx context.Context, id string, decision M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/reports/"+q(id)+":resolve", decision, &out)
}

// SilenceMember stops a member from sending, editing or streaming in the
// conversation for seconds.
func (c *Client) SilenceMember(ctx context.Context, convID, userID string, seconds int, reason string) (M, error) {
	var out M
	body := M{"seconds": seconds}
	if reason != "" {
		body["reason"] = reason
	}
	return out, c.Do(ctx, "POST", "/v1/conversations/"+q(convID)+"/members/"+q(userID)+":silence", body, &out)
}

func (c *Client) UnsilenceMember(ctx context.Context, convID, userID string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/conversations/"+q(convID)+"/members/"+q(userID)+":unsilence", nil, &out)
}

// ---- users

func (c *Client) UpsertUser(ctx context.Context, id string, attrs M) (M, error) {
	var out M
	if attrs == nil {
		attrs = M{}
	}
	return out, c.Do(ctx, "PUT", "/v1/users/"+q(id), attrs, &out)
}

func (c *Client) GetUser(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/users/"+q(id), nil, &out)
}

func (c *Client) UpdateUser(ctx context.Context, id string, attrs M) (M, error) {
	var out M
	return out, c.Do(ctx, "PATCH", "/v1/users/"+q(id), attrs, &out)
}

func (c *Client) DeactivateUser(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/users/"+q(id)+":deactivate", nil, &out)
}

func (c *Client) ReactivateUser(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/users/"+q(id)+":reactivate", nil, &out)
}

func (c *Client) BatchUsers(ctx context.Context, users []M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/users:batch", M{"users": users}, &out)
}

func (c *Client) UserConversations(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/users/"+q(id)+"/conversations", nil, &out)
}

// ---- conversations; `as` is a user id or "system"

func (c *Client) CreateConversation(ctx context.Context, conv M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/conversations", conv, &out)
}

func (c *Client) GetConversation(ctx context.Context, id string) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", "/v1/conversations/"+q(id), nil, &out)
}

func (c *Client) UpdateConversation(ctx context.Context, id string, change M) (M, error) {
	var out M
	return out, c.Do(ctx, "PATCH", "/v1/conversations/"+q(id), change, &out)
}

func (c *Client) DissolveConversation(ctx context.Context, id, as string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/conversations/"+q(id)+"?as="+url.QueryEscape(as), nil, &out)
}

func (c *Client) AddMembers(ctx context.Context, id, as string, users []string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/conversations/"+q(id)+"/members", M{"as": as, "users": users}, &out)
}

func (c *Client) RemoveMember(ctx context.Context, id, user, as string) (M, error) {
	var out M
	return out, c.Do(ctx, "DELETE", "/v1/conversations/"+q(id)+"/members/"+q(user)+"?as="+url.QueryEscape(as), nil, &out)
}

func (c *Client) SetRole(ctx context.Context, id, user, role, as string) (M, error) {
	var out M
	return out, c.Do(ctx, "PATCH", "/v1/conversations/"+q(id)+"/members/"+q(user), M{"as": as, "role": role}, &out)
}

// ---- messages; m holds as, text and optionally kind, payload,
// client_msg_id, reply_to, to_user

// UploadFile uploads a whole file (at most 64 MiB) as user `as`: the server
// removes EXIF and GPS and makes a thumbnail. mime may be empty.
func (c *Client) UploadFile(ctx context.Context, convID, as, name string, data []byte, mime string) (M, error) {
	qs := url.Values{"as": {as}, "name": {name}}
	if mime != "" {
		qs.Set("mime", mime)
	}
	var out M
	return out, c.DoRaw(ctx, "POST", "/v1/conversations/"+q(convID)+"/files?"+qs.Encode(), data, &out)
}

// SendFile uploads a file and sends it as an image or file message.
func (c *Client) SendFile(ctx context.Context, convID, as, name string, data []byte, mime, caption string) (M, error) {
	f, err := c.UploadFile(ctx, convID, as, name, data, mime)
	if err != nil {
		return nil, err
	}
	kind, text := "file", fmt.Sprintf("[File] %v", f["name"])
	if f["kind"] == "image" {
		kind, text = "image", "[Image]"
	}
	payload := M{"file_id": f["file_id"]}
	if caption != "" {
		payload["caption"] = caption
	}
	out, err := c.SendMessage(ctx, convID, M{"as": as, "text": text, "kind": kind, "payload": payload})
	if err != nil {
		return nil, err
	}
	out["file"] = f
	return out, nil
}

// FileURL is a link (valid 5 minutes) to read a file; variant is "original",
// "thumb" or empty, as may be empty (the system).
func (c *Client) FileURL(ctx context.Context, convID, fileID, as, variant string) (M, error) {
	qs := url.Values{}
	if as != "" {
		qs.Set("as", as)
	}
	if variant != "" {
		qs.Set("variant", variant)
	}
	var out M
	return out, c.Do(ctx, "GET", "/v1/conversations/"+q(convID)+"/files/"+q(fileID)+"?"+qs.Encode(), nil, &out)
}

func (c *Client) SendMessage(ctx context.Context, convID string, m M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/conversations/"+q(convID)+"/messages", m, &out)
}

func (c *Client) SendToUser(ctx context.Context, userID string, m M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/users/"+q(userID)+"/messages", m, &out)
}

func (c *Client) BatchSend(ctx context.Context, m M) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/messages:batch-send", m, &out)
}

func (c *Client) EditMessage(ctx context.Context, id string, m M) (M, error) {
	var out M
	return out, c.Do(ctx, "PATCH", "/v1/messages/"+q(id), m, &out)
}

func (c *Client) RecallMessage(ctx context.Context, id, as string) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/messages/"+q(id)+":recall", M{"as": as}, &out)
}

func (c *Client) History(ctx context.Context, convID string, afterSeq, limit int) (M, error) {
	var out M
	return out, c.Do(ctx, "GET", fmt.Sprintf("/v1/conversations/%s/messages?after_seq=%d&limit=%d", q(convID), afterSeq, limit), nil, &out)
}

func (c *Client) MarkRead(ctx context.Context, convID, as string, seq int) (M, error) {
	var out M
	return out, c.Do(ctx, "POST", "/v1/conversations/"+q(convID)+"/read", M{"as": as, "seq": seq}, &out)
}
