package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"github.com/infinityabundance/entropyfs/go/entropyfs"
)

// The content-store smoke: the example's routes exercised in-process
// against a real engine — PUT, GET (full + range), 404 on a missing
// id, and metrics.

func TestContentStoreSmoke(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "store")
	e, err := entropyfs.Create(dir, entropyfs.OpenOptions{})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	defer e.Close()
	srv := httptest.NewServer(newServer(e))
	defer srv.Close()

	payload := bytes.Repeat([]byte("content-store-payload-"), 200)

	// PUT
	req, err := http.NewRequest(http.MethodPut, srv.URL+"/blob", bytes.NewReader(payload))
	if err != nil {
		t.Fatalf("PUT request: %v", err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("PUT: %v", err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("PUT status %d: %s", resp.StatusCode, body)
	}
	var put struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(body, &put); err != nil || len(put.ID) != 64 {
		t.Fatalf("PUT response: %q err=%v", body, err)
	}

	// GET full
	resp, err = http.Get(srv.URL + "/blob/" + put.ID)
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	got, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK || !bytes.Equal(got, payload) {
		t.Fatalf("GET full: status=%d bytes=%d", resp.StatusCode, len(got))
	}

	// GET range
	resp, err = http.Get(srv.URL + "/blob/" + put.ID + "?offset=100&length=64")
	if err != nil {
		t.Fatalf("GET range: %v", err)
	}
	got, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK || !bytes.Equal(got, payload[100:164]) {
		t.Fatalf("GET range: status=%d bytes=%d", resp.StatusCode, len(got))
	}

	// GET missing -> 404
	resp, err = http.Get(srv.URL + "/blob/" + "0000000000000000000000000000000000000000000000000000000000000000")
	if err != nil {
		t.Fatalf("GET missing: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("GET missing status = %d, want 404", resp.StatusCode)
	}

	// metrics
	resp, err = http.Get(srv.URL + "/metrics")
	if err != nil {
		t.Fatalf("GET metrics: %v", err)
	}
	got, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK || !bytes.Contains(got, []byte("schema_version")) {
		t.Fatalf("GET metrics: status=%d body=%d bytes", resp.StatusCode, len(got))
	}

	// durability + lifecycle: the engine closes cleanly (reopen-survival
	// is covered by the binding's own court).
	if err := e.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}
