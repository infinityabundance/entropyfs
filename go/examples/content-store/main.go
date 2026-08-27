// Command content-store is a deliberately thin concurrent content-
// addressed object service on the EntropyFS engine (Phase 12E.15).
//
// # Why it exists
//
// It demonstrates that an infrastructure engineer can embed the engine
// as a storage backend WITHOUT mounting FUSE, writing Rust, or parsing
// native error strings. It is NOT a new network protocol for EntropyFS:
// the surface is exactly the Go binding's PutBlob / GetBlob /
// ReadBlobRange / Metrics.
//
// # Routes
//
//	PUT /blob (body)             -> {"id": "<64 hex>"}
//	GET /blob/{id}               -> the blob's bytes
//	GET /blob/{id}?offset=&length= -> a byte range
//	GET /metrics                 -> the engine metrics JSON
//
// # Concurrency
//
// The engine handle is safe for concurrent use; the HTTP handlers call
// it directly (no locking in this example).
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strconv"
	"time"

	"github.com/infinityabundance/entropyfs/go/entropyfs"
)

func main() {
	storeDir := flag.String("store", "/tmp/efs-content-store", "store directory (created if absent)")
	addr := flag.String("addr", ":8080", "listen address")
	flag.Parse()

	var engine *entropyfs.Engine
	var err error
	if _, statErr := os.Stat(*storeDir); statErr == nil {
		engine, err = entropyfs.Open(*storeDir, entropyfs.OpenOptions{})
	} else {
		engine, err = entropyfs.Create(*storeDir, entropyfs.OpenOptions{})
	}
	if err != nil {
		log.Fatalf("open/create engine: %v", err)
	}
	defer engine.Close()

	srv := &http.Server{
		Addr:              *addr,
		Handler:           newServer(engine),
		ReadHeaderTimeout: 5 * time.Second,
	}
	log.Printf("content-store: %s serving %s", *addr, *storeDir)
	if err := srv.ListenAndServe(); err != nil {
		log.Fatal(err)
	}
}

// newServer wires the routes onto the engine. Exposed for the
// in-process smoke test.
func newServer(e *entropyfs.Engine) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("PUT /blob", func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, "read body: "+err.Error(), http.StatusBadRequest)
			return
		}
		id, err := e.PutBlob(body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"id": id.String()})
	})
	mux.HandleFunc("GET /blob/{id}", func(w http.ResponseWriter, r *http.Request) {
		id, err := parseID(r.PathValue("id"))
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		q := r.URL.Query()
		if q.Has("offset") || q.Has("length") {
			offset, _ := strconv.ParseInt(q.Get("offset"), 10, 64)
			length, _ := strconv.Atoi(q.Get("length"))
			data, err := e.ReadBlobRange(id, offset, length)
			if err != nil {
				http.Error(w, err.Error(), statusFor(err))
				return
			}
			w.Write(data)
			return
		}
		data, err := e.GetBlob(id)
		if err != nil {
			http.Error(w, err.Error(), statusFor(err))
			return
		}
		w.Write(data)
	})
	mux.HandleFunc("GET /metrics", func(w http.ResponseWriter, r *http.Request) {
		m, err := e.Metrics()
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Write(m.Raw)
	})
	return mux
}

func parseID(s string) (entropyfs.BlobID, error) {
	var id entropyfs.BlobID
	if len(s) != 64 {
		return id, fmt.Errorf("blob id must be 64 hex characters")
	}
	for i := 0; i < 32; i++ {
		hi, ok1 := hexVal(s[i*2])
		lo, ok2 := hexVal(s[i*2+1])
		if !ok1 || !ok2 {
			return id, fmt.Errorf("invalid hex in blob id")
		}
		id[i] = hi<<4 | lo
	}
	return id, nil
}

func hexVal(c byte) (byte, bool) {
	switch {
	case c >= '0' && c <= '9':
		return c - '0', true
	case c >= 'a' && c <= 'f':
		return c - 'a' + 10, true
	case c >= 'A' && c <= 'F':
		return c - 'A' + 10, true
	}
	return 0, false
}

func statusFor(err error) int {
	// The stable classes map to HTTP statuses (errors.Is compares the
	// CODE, not the message); messages stay out of the status decision.
	switch {
	case errors.Is(err, entropyfs.ErrNotFound):
		return http.StatusNotFound
	case errors.Is(err, entropyfs.ErrInvalidArgument):
		return http.StatusBadRequest
	default:
		return http.StatusInternalServerError
	}
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}
