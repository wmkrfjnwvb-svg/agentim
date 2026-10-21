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
	var data []byte
	if body != nil {
		var err error
		if data, err = json.Marshal(clean(body)); err != nil {
			return err
		}
	}
	idem := newKey()
	retryable := method == http.MethodGet || method == http.MethodPut || method == http.MethodPost
	for attempt := 0; ; attempt++ {
		req, err := http.NewRequestWithContext(ctx, method, c.BaseURL+path, bytes.NewReader(data))
		if err != nil {
			return err
		}
		req.Header.Set("authorization", "Bearer "+c.APIKey)
		if body != nil {
			req.Header.Set("content-type", "application/json")
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
