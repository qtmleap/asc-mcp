package reviewer

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testClient(t *testing.T, h http.HandlerFunc) *Client {
	t.Helper()
	s := httptest.NewServer(h)
	t.Cleanup(s.Close)
	p := filepath.Join(t.TempDir(), "cookie")
	os.WriteFile(p, []byte("---\n- !ruby/object:HTTP::Cookie\n  name: myacinfo\n  value: PRIVATE_SESSION\n  domain: .apple.com\n  path: /\n  secure: true\n"), 0600)
	c := New(p)
	c.origin = s.URL
	c.httpClient = s.Client()
	return c
}
func TestThreadsAndMessages(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Cookie") != "myacinfo=PRIVATE_SESSION" {
			t.Error("missing session")
		}
		if r.Header.Get("Authorization") != "" {
			t.Error("JWT must not enter reviewer requests")
		}
		switch r.URL.Path {
		case "/iris/v1/resolutionCenterThreads":
			if r.URL.Query().Get("filter[reviewSubmission]") != "sub-1" {
				t.Error("wrong submission")
			}
		case "/iris/v1/resolutionCenterThreads/thread-1/resolutionCenterMessages":
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"data":[]}`))
	})
	if _, err := c.Threads(context.Background(), "sub-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := c.Messages(context.Background(), "thread-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := c.Messages(context.Background(), "../apps"); err == nil {
		t.Fatal("accepted traversal")
	}
}
func TestAuthAndRedaction(t *testing.T) {
	for _, status := range []int{401, 403, 302} {
		t.Run(http.StatusText(status), func(t *testing.T) {
			c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Location", "https://example.com")
				w.WriteHeader(status)
				w.Write([]byte("PRIVATE_SESSION"))
			})
			_, err := c.Threads(context.Background(), "sub-1")
			if err == nil || strings.Contains(err.Error(), "PRIVATE_SESSION") {
				t.Fatalf("unsafe error: %v", err)
			}
		})
	}
}
func TestCookieReloadAndPermissions(t *testing.T) {
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"data":[]}`))
	})
	os.Chmod(c.cookiePath, 0644)
	if _, err := c.Threads(context.Background(), "sub-1"); err == nil {
		t.Fatal("accepted world-readable session")
	}
	os.Chmod(c.cookiePath, 0600)
	os.WriteFile(c.cookiePath, []byte("---\n- name: myacinfo\n  value: expired\n  domain: .apple.com\n  path: /\n  expires: 2000-01-01 00:00:00.000000000 Z\n"), 0600)
	if _, err := c.Threads(context.Background(), "sub-1"); err == nil {
		t.Fatal("accepted expired session")
	}
}
func TestReplyPreflightAndOnePost(t *testing.T) {
	posts := 0
	c := testClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.Method == "GET" {
			w.Header().Set("csrf", "csrf-test")
			w.Header().Set("csrf_ts", "csrf-time")
			w.Write([]byte(`{"statusCode":"SUCCESS","data":{"appNotes":{"threads":[{"id":"t1","versionId":"v1","version":"1.0","active":true,"canDeveloperAddNote":true,"messages":[]}]}}}`))
			return
		}
		posts++
		if r.Header.Get("csrf") != "csrf-test" {
			t.Error("missing CSRF")
		}
		var body map[string]any
		json.NewDecoder(r.Body).Decode(&body)
		if body["appNotes"] == nil {
			t.Error("wrong body")
		}
		w.WriteHeader(503)
		w.Write([]byte("PRIVATE_SESSION"))
	})
	if _, err := c.Reply(context.Background(), "123", "ios", "t1", "Developer", "Hello"); err == nil || !strings.Contains(err.Error(), "delivery_unknown") {
		t.Fatalf("expected uncertain result: %v", err)
	}
	if posts != 1 {
		t.Fatalf("POST repeated %d times", posts)
	}
	if _, err := c.Reply(context.Background(), "123", "ios", "t1", "Developer", strings.Repeat("🙂", 2001)); err == nil {
		t.Fatal("accepted overlong reply")
	}
	if posts != 1 {
		t.Fatal("invalid text sent")
	}
}
