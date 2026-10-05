//go:build migration_export

// Offline migration fixture exporter. The fake transport always returns a terminal
// 400 response; it cannot make network calls or mutate an Apple account.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"github.com/antisynthesis/asc-mcp/internal/asc/api"
	"github.com/antisynthesis/asc-mcp/internal/asc/mcp"
	"github.com/antisynthesis/asc-mcp/internal/asc/tools"
	"io"
	"net/http"
	"os"
	"strings"
)

type request struct {
	Method string              `json:"method"`
	Path   string              `json:"path"`
	Query  map[string][]string `json:"query"`
	Body   any                 `json:"body"`
}
type transport struct{ Requests []request }

func (t *transport) RoundTrip(r *http.Request) (*http.Response, error) {
	var b any
	if r.Body != nil {
		bytes, _ := io.ReadAll(r.Body)
		json.Unmarshal(bytes, &b)
	}
	t.Requests = append(t.Requests, request{r.Method, r.URL.EscapedPath(), r.URL.Query(), b})
	return &http.Response{StatusCode: 400, Header: http.Header{}, Body: io.NopCloser(strings.NewReader(`{"errors":[{"title":"Offline capture","detail":"Intentional terminal capture response"}]}`)), Request: r}, nil
}

type probe struct {
	Args     map[string]any `json:"args"`
	Requests []request      `json:"requests"`
	Result   any            `json:"result,omitempty"`
	Error    string         `json:"error,omitempty"`
}

func sample(name string, p mcp.Property) any {
	if name == "properties" {
		return map[string]string{"migration": "MIG_properties.migration"}
	}
	if name == "cursor" || name == "file_path" {
		return nil
	}
	if name == "file_data_base64" {
		return "dGVzdA=="
	}
	switch p.Type {
	case "integer", "number":
		return 7
	case "boolean":
		return true
	case "array":
		if p.Items != nil && p.Items.Type == "object" {
			return []any{object(name)}
		}
		return []string{"MIG_" + name + "_0", "MIG_" + name + "_1"}
	case "object":
		if name == "filter" || name == "fields" {
			return map[string]any{"migration": []string{"MIG_" + name + "_0", "MIG_" + name + "_1"}}
		}
		return object(name)
	default:
		if len(p.Enum) > 0 {
			return p.Enum[0]
		}
		if strings.Contains(name, "date") {
			return "2026-10-01"
		}
		return "MIG_" + name
	}
}
func object(name string) map[string]any {
	m := map[string]any{}
	for _, k := range []string{"territory_id", "price_point_id", "start_date", "end_date", "currency", "customer_price", "proceeds", "subscription_price_point_id", "app_price_point_id", "is_primary", "preserve_current_price", "manual", "type", "id", "name", "url"} {
		if k == "preserve_current_price" || k == "manual" || k == "is_primary" {
			m[k] = true
		} else if strings.Contains(k, "date") {
			m[k] = "MIG_" + name + "." + k
		} else {
			m[k] = "MIG_" + name + "." + k
		}
	}
	return m
}
func clone(v map[string]any) map[string]any {
	b, _ := json.Marshal(v)
	var m map[string]any
	json.Unmarshal(b, &m)
	return m
}
func run(r *tools.Registry, t *transport, name string, a map[string]any) probe {
	t.Requests = nil
	b, _ := json.Marshal(a)
	res, err := r.CallTool(context.Background(), name, b)
	p := probe{Args: a, Requests: append([]request{}, t.Requests...), Result: res}
	if err != nil {
		p.Error = err.Error()
	}
	return p
}
func main() {
	t := &transport{}
	r := tools.NewRegistry(api.MigrationCaptureClient(t))
	schema := r.ListTools()
	b, _ := json.MarshalIndent(schema, "", "  ")
	os.WriteFile("data/public_tools.json", b, 0644)
	actual := handlerArguments()
	b, _ = json.MarshalIndent(actual, "", "  ")
	os.WriteFile("data/handler_arguments.json", b, 0644)
	out := map[string]any{}
	for _, tool := range schema {
		for key, p := range actual[tool.Name] {
			if _, exists := tool.InputSchema.Properties[key]; !exists {
				tool.InputSchema.Properties[key] = p
			}
		}
		a := map[string]any{}
		for k, p := range tool.InputSchema.Properties {
			if v := sample(k, p); v != nil {
				a[k] = v
			}
		}
		if tool.Name == "add_review_submission_item" {
			for _, k := range []string{"app_custom_product_page_version_id", "app_store_version_experiment_id", "app_event_id", "game_center_achievement_version_id", "game_center_leaderboard_version_id", "game_center_leaderboard_set_version_id", "game_center_activity_version_id", "game_center_challenge_version_id", "in_app_purchase_version_id", "subscription_version_id", "subscription_group_version_id"} {
				delete(a, k)
			}
		}
		if strings.HasPrefix(tool.Name, "create_game_center_") {
			delete(a, "game_center_group_id")
		}
		if tool.Name == "upload_game_center_activity_image" || tool.Name == "upload_game_center_challenge_image" {
			delete(a, "version_id")
		}
		probes := map[string]probe{"full": run(r, t, tool.Name, a)}
		req := clone(a)
		for k := range req {
			required := false
			for _, n := range tool.InputSchema.Required {
				if k == n {
					required = true
				}
			}
			if !required {
				delete(req, k)
			}
		}
		probes["required"] = run(r, t, tool.Name, req)
		for k, p := range tool.InputSchema.Properties {
			for _, choice := range p.Enum {
				e := clone(a)
				e[k] = choice
				probes["enum:"+k+":"+choice] = run(r, t, tool.Name, e)
			}
			if k == "price_type" {
				e := clone(a)
				e[k] = "AUTOMATIC"
				probes["enum:"+k+":AUTOMATIC"] = run(r, t, tool.Name, e)
			}
			if k == "filter" || k == "fields" {
				e := clone(a)
				m := map[string][]string{}
				for _, key := range []string{"migration", "app", "build", "betaGroups", "upfrontPricePointId", "planType"} {
					m[key] = []string{"COLLISION_" + key}
				}
				e[k] = m
				probes["collision:"+k] = run(r, t, tool.Name, e)
			}
			if k == "cursor" {
				e := clone(a)
				e[k] = "https://api.appstoreconnect.apple.com/v1/apps?cursor=migration"
				probes["cursor"] = run(r, t, tool.Name, e)
			}
			empty := clone(a)
			switch p.Type {
			case "string":
				empty[k] = ""
			case "array":
				empty[k] = []any{}
			case "object":
				empty[k] = map[string]any{}
			default:
				empty[k] = nil
			}
			probes["empty:"+k] = run(r, t, tool.Name, empty)
			omit := clone(a)
			delete(omit, k)
			probes["omit:"+k] = run(r, t, tool.Name, omit)
			if p.Type == "array" && p.Items != nil && p.Items.Type == "object" {
				for _, subkey := range []string{"start_date", "end_date", "price_point_id", "territory_id"} {
					z := clone(a)
					o := object(k)
					delete(o, subkey)
					z[k] = []any{o}
					probes["nested_omit:"+k+":"+subkey] = run(r, t, tool.Name, z)
				}
				for _, subkey := range []string{"start_date", "end_date"} {
					z := clone(a)
					o := object(k)
					o[subkey] = ""
					z[k] = []any{o}
					probes["nested_empty:"+k+":"+subkey] = run(r, t, tool.Name, z)
				}
				z := clone(a)
				z[k] = []any{map[string]string{"price_point_id": "minimal-price", "territory_id": "minimal-territory"}}
				probes["minimal:"+k] = run(r, t, tool.Name, z)
				z = clone(a)
				z[k] = []any{object(k), object(k), object(k)}
				probes["many:"+k] = run(r, t, tool.Name, z)
			}
			alt := clone(a)
			switch p.Type {
			case "boolean":
				alt[k] = false
			case "integer", "number":
				alt[k] = 11
			case "array":
				if p.Items != nil && p.Items.Type == "object" {
					continue
				}
				alt[k] = []string{"ALT_" + k}
			case "object":
				continue
			default:
				if k == "cursor" || strings.Contains(k, "file_") {
					continue
				}
				alt[k] = "ALT_" + k
			}
			if k == "game_center_group_id" {
				delete(alt, "game_center_detail_id")
			}
			if k == "version_id" && strings.HasPrefix(tool.Name, "upload_game_center_") {
				delete(alt, "localization_id")
			}
			if tool.Name == "add_review_submission_item" && k != "review_submission_id" {
				for other := range a {
					if other != "review_submission_id" && other != k {
						delete(alt, other)
					}
				}
			}
			probes["alt:"+k] = run(r, t, tool.Name, alt)
			if p.Type == "integer" || p.Type == "number" {
				z := clone(a)
				z[k] = 0
				probes["zero:"+k] = run(r, t, tool.Name, z)
				z = clone(a)
				z[k] = 1001
				probes["high:"+k] = run(r, t, tool.Name, z)
			}
			if p.Type == "boolean" {
				z := clone(a)
				z[k] = nil
				probes["null:"+k] = run(r, t, tool.Name, z)
			}
		}
		out[tool.Name] = probes
		if len(probes["full"].Requests) == 0 {
			fmt.Println("NO REQUEST", tool.Name, string(mustJSON(probes["full"].Result)), probes["full"].Error)
		}
	}
	b, _ = json.MarshalIndent(out, "", "  ")
	os.WriteFile("data/go_request_probes.json", b, 0644)
}
func mustJSON(v any) []byte { b, _ := json.Marshal(v); return b }
