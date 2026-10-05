package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestMonitorInvocationAndAcknowledgement(t *testing.T) {
	path := filepath.Join(t.TempDir(), "monitor")
	os.WriteFile(path, []byte("#!/bin/sh\nif [ \"${1:-}\" = '--ack' ]; then echo '{\"status\":\"acknowledged\"}'; else echo '{\"status\":\"unchanged\"}'; fi\n"), 0700)
	r := NewReviewerRegistry("/nonexistent", false)
	r.AddReviewerMonitor(path)
	for _, name := range []string{"check_reviewer_updates", "acknowledge_reviewer_updates"} {
		result, err := r.CallTool(context.Background(), name, func() json.RawMessage {
			if name == "acknowledge_reviewer_updates" {
				return json.RawMessage(`{"snapshot_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}`)
			}
			return json.RawMessage(`{}`)
		}())
		if err != nil || result.IsError {
			t.Fatalf("monitor call failed: %v", err)
		}
	}
	bad, _ := r.CallTool(context.Background(), "check_reviewer_updates", json.RawMessage(`{"command":"bad"}`))
	if !bad.IsError {
		t.Fatal("accepted executable injection")
	}
	os.WriteFile(path, []byte("#!/bin/sh\necho '{\"status\":\"blocked\",\"reason\":\"authentication_required\"}'\nexit 2\n"), 0700)
	result, err := r.CallTool(context.Background(), "check_reviewer_updates", json.RawMessage(`{}`))
	if err != nil || !result.IsError || !strings.Contains(result.Content[0].Text, "authentication_required") {
		t.Fatal("authentication failure swallowed")
	}
}
