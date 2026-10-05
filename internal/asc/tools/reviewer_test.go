package tools

import (
	"context"
	"encoding/json"
	"testing"
)

func TestReviewerOnlyRegistration(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		r := NewReviewerRegistry("/nonexistent/cookie", enabled)
		want := 3
		if enabled {
			want = 4
		}
		if len(r.ListTools()) != want {
			t.Fatalf("expected %d tools", want)
		}
		for _, tool := range r.ListTools() {
			if tool.Name == "reply_reviewer_message" && *tool.Annotations.ReadOnlyHint {
				t.Fatal("reply must not be read-only")
			}
		}
		result, err := r.CallTool(context.Background(), "get_reviewer_messages", json.RawMessage(`{"thread_id":"t1"}`))
		if err != nil || !result.IsError {
			t.Fatal("missing session must fail")
		}
		if _, err := r.CallTool(context.Background(), "list_apps", json.RawMessage(`{}`)); err == nil {
			t.Fatal("exposed public API tool without client")
		}
	}
}
