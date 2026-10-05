// Package reviewer accesses Apple's undocumented reviewer APIs with a CLI-created
// Apple ID session. It does not use the public API's JWT or control a browser.
package reviewer

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strings"
	"time"
	"unicode/utf16"

	"go.yaml.in/yaml/v2"
)

const maxResponse = 8 << 20

var identifier = regexp.MustCompile(`^[A-Za-z0-9_-]+$`)

type Client struct {
	cookiePath string
	origin     string
	httpClient *http.Client
}

func New(cookiePath string) *Client {
	return &Client{cookiePath: cookiePath, origin: "https://appstoreconnect.apple.com", httpClient: &http.Client{Timeout: 30 * time.Second}}
}

type storedCookie struct {
	Name    string `yaml:"name"`
	Value   string `yaml:"value"`
	Domain  string `yaml:"domain"`
	Path    string `yaml:"path"`
	Expires string `yaml:"expires"`
}

func (c *Client) cookies() (string, error) {
	f, err := os.Open(c.cookiePath)
	if err != nil {
		return "", fmt.Errorf("reviewer_session_required: authenticate with fastlane spaceauth and configure ASC_REVIEW_COOKIE_PATH")
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil || !st.Mode().IsRegular() || st.Mode().Perm()&0077 != 0 {
		return "", fmt.Errorf("reviewer_session_permissions: session must be a regular file readable only by its owner (chmod 600)")
	}
	b, err := io.ReadAll(io.LimitReader(f, 1<<20))
	if err != nil {
		return "", fmt.Errorf("reviewer_session_unreadable")
	}
	var entries []storedCookie
	if yaml.Unmarshal(b, &entries) != nil {
		return "", fmt.Errorf("reviewer_session_invalid: expected fastlane HTTP::CookieJar YAML")
	}
	var cookies []string
	for _, v := range entries {
		domain := strings.TrimPrefix(v.Domain, ".")
		if domain != "apple.com" && domain != "appstoreconnect.apple.com" {
			continue
		}
		if v.Path != "" && v.Path != "/" {
			continue
		}
		if v.Expires != "" {
			var expiry time.Time
			for _, layout := range []string{time.RFC3339Nano, "2006-01-02 15:04:05.999999999 Z07:00", "2006-01-02 15:04:05.999999999 Z"} {
				if parsed, e := time.Parse(layout, v.Expires); e == nil {
					expiry = parsed
					break
				}
			}
			if expiry.IsZero() || !expiry.After(time.Now()) {
				continue
			}
		}
		cookie := &http.Cookie{Name: v.Name, Value: v.Value}
		if cookie.Valid() != nil || v.Value == "" {
			continue
		}
		cookies = append(cookies, cookie.String())
	}
	if len(cookies) == 0 {
		return "", fmt.Errorf("reviewer_session_required: no unexpired App Store Connect cookies; repeat CLI authentication")
	}
	return strings.Join(cookies, "; "), nil
}

func (c *Client) request(ctx context.Context, method, path string, q url.Values, payload any, csrf http.Header) (json.RawMessage, http.Header, error) {
	cookie, err := c.cookies()
	if err != nil {
		return nil, nil, err
	}
	var body []byte
	if payload != nil {
		body, err = json.Marshal(payload)
		if err != nil {
			return nil, nil, fmt.Errorf("reviewer_request_invalid")
		}
	}
	u := c.origin + path
	if len(q) > 0 {
		u += "?" + q.Encode()
	}
	req, err := http.NewRequestWithContext(ctx, method, u, bytes.NewReader(body))
	if err != nil {
		return nil, nil, fmt.Errorf("reviewer_request_invalid")
	}
	req.Header.Set("Cookie", cookie)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("Content-Type", "application/json")
	for _, k := range []string{"csrf", "csrf_ts"} {
		if v := csrf.Get(k); v != "" {
			req.Header.Set(k, v)
		}
	}
	// Never follow a login or cross-origin redirect with the Apple session.
	transport := *c.httpClient
	transport.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	resp, err := transport.Do(req)
	if err != nil {
		if method == http.MethodPost {
			return nil, nil, fmt.Errorf("delivery_unknown: reply request interrupted; read the thread before retrying")
		}
		return nil, nil, fmt.Errorf("reviewer_request_failed: HTTP connection failed")
	}
	defer resp.Body.Close()
	if resp.StatusCode == 401 || resp.StatusCode == 403 || resp.StatusCode/100 == 3 {
		return nil, nil, fmt.Errorf("reviewer_auth_required: Apple ID session or permissions rejected; repeat CLI authentication")
	}
	if resp.StatusCode/100 != 2 {
		if method == http.MethodPost && resp.StatusCode >= 500 {
			return nil, nil, fmt.Errorf("delivery_unknown: Apple returned HTTP %d; read the thread before retrying", resp.StatusCode)
		}
		return nil, nil, fmt.Errorf("reviewer_api_unavailable: HTTP %d (undocumented endpoint; not a successful response)", resp.StatusCode)
	}
	b, err := io.ReadAll(io.LimitReader(resp.Body, maxResponse+1))
	if err != nil || len(b) > maxResponse || !json.Valid(b) {
		if method == http.MethodPost {
			return nil, nil, fmt.Errorf("delivery_unknown: invalid reply response; read the thread before retrying")
		}
		return nil, nil, fmt.Errorf("reviewer_response_invalid: expected bounded JSON")
	}
	return json.RawMessage(b), resp.Header, nil
}
func validID(s string) bool { return identifier.MatchString(s) }
func (c *Client) Threads(ctx context.Context, submissionID string) (json.RawMessage, error) {
	if !validID(submissionID) {
		return nil, fmt.Errorf("invalid review_submission_id")
	}
	b, _, err := c.request(ctx, "GET", "/iris/v1/resolutionCenterThreads", url.Values{"filter[reviewSubmission]": {submissionID}, "limit": {"200"}}, nil, nil)
	return b, err
}
func (c *Client) Messages(ctx context.Context, threadID string) (json.RawMessage, error) {
	if !validID(threadID) {
		return nil, fmt.Errorf("invalid thread_id")
	}
	b, _, err := c.request(ctx, "GET", "/iris/v1/resolutionCenterThreads/"+threadID+"/resolutionCenterMessages", url.Values{"include": {"fromActor,rejections,resolutionCenterMessageAttachments"}, "limit": {"200"}}, nil, nil)
	return b, err
}

// Reply uses the legacy route still present in fastlane's Tunes client.
// It is experimental, opt-in, and must be validated with a real account.
func (c *Client) Reply(ctx context.Context, appID, platform, threadID, sender, body string) (json.RawMessage, error) {
	if !validID(appID) || !validID(threadID) {
		return nil, fmt.Errorf("invalid app_id or thread_id")
	}
	switch platform {
	case "ios", "osx", "appletvos", "xros":
	default:
		return nil, fmt.Errorf("invalid platform")
	}
	if strings.TrimSpace(body) == "" || len(utf16.Encode([]rune(body))) > 4000 || strings.TrimSpace(sender) == "" {
		return nil, fmt.Errorf("sender and nonempty message (at most 4000 UTF-16 units) required")
	}
	path := "/WebObjects/iTunesConnect.woa/ra/apps/" + appID + "/platforms/" + platform + "/resolutionCenter"
	data, headers, err := c.request(ctx, "GET", path, url.Values{"v": {"latest"}}, nil, nil)
	if err != nil {
		return nil, err
	}
	var current struct {
		Status string `json:"statusCode"`
		Data   struct {
			Notes struct {
				Threads []struct {
					ID        string `json:"id"`
					VersionID string `json:"versionId"`
					Version   string `json:"version"`
					Active    bool   `json:"active"`
					CanReply  bool   `json:"canDeveloperAddNote"`
					Messages  []struct {
						Body string `json:"body"`
					} `json:"messages"`
				} `json:"threads"`
			} `json:"appNotes"`
		} `json:"data"`
	}
	if json.Unmarshal(data, &current) != nil || current.Status != "SUCCESS" {
		return nil, fmt.Errorf("reviewer_reply_preflight_failed: unrecognized legacy thread response")
	}
	for _, thread := range current.Data.Notes.Threads {
		if thread.ID != threadID {
			continue
		}
		if !thread.Active || !thread.CanReply || thread.VersionID == "" || thread.Version == "" {
			return nil, fmt.Errorf("reviewer_thread_not_replyable")
		}
		for _, message := range thread.Messages {
			if message.Body == body {
				return nil, fmt.Errorf("reviewer_duplicate_reply: identical body already present; not sent")
			}
		}
		if headers.Get("csrf") == "" || headers.Get("csrf_ts") == "" {
			return nil, fmt.Errorf("reviewer_csrf_required: preflight did not supply both CSRF headers; not sent")
		}
		payload := map[string]any{"appNotes": map[string]any{"threads": []any{map[string]any{"id": thread.ID, "versionId": thread.VersionID, "version": thread.Version, "messages": []any{map[string]any{"from": sender, "date": fmt.Sprint(time.Now().UnixMilli()), "body": body, "tokens": []any{}}}}}}}
		result, _, err := c.request(ctx, "POST", path, nil, payload, headers)
		if err != nil {
			return nil, err
		}
		var reply struct {
			Status string `json:"statusCode"`
		}
		if json.Unmarshal(result, &reply) != nil || reply.Status != "SUCCESS" {
			return nil, fmt.Errorf("delivery_unknown: Apple did not return legacy SUCCESS; read the thread before retrying")
		}
		// Acceptance is not a read-back confirmation of delivery.
		return json.RawMessage(`{"status":"accepted_unverified","message":"Apple returned SUCCESS; read reviewer messages to confirm delivery. Do not automatically resend."}`), nil
	}
	return nil, fmt.Errorf("reviewer_thread_not_found: thread was not found in this app's legacy reply context")
}

// LegacyContext exposes the replyable thread identifiers provided by Apple's
// legacy endpoint. Iris thread IDs must not be assumed to be interchangeable.
func (c *Client) LegacyContext(ctx context.Context, appID, platform string) (json.RawMessage, error) {
	if !validID(appID) {
		return nil, fmt.Errorf("invalid app_id")
	}
	switch platform {
	case "ios", "osx", "appletvos", "xros":
	default:
		return nil, fmt.Errorf("invalid platform")
	}
	b, _, err := c.request(ctx, "GET", "/WebObjects/iTunesConnect.woa/ra/apps/"+appID+"/platforms/"+platform+"/resolutionCenter", url.Values{"v": {"latest"}}, nil, nil)
	return b, err
}
