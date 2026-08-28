package entropyfs

import (
	"bytes"
	"errors"
	"path/filepath"
	"testing"
)

// The correctness + exactness court: the Go binding must behave exactly
// like the Rust Engine API (the C ABI is the only path). Every blob is
// verified byte-for-byte.

func mustCreate(t *testing.T) (*Engine, string) {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "store")
	e, err := Create(dir, OpenOptions{})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	return e, dir
}

func TestPutGetRoundTrip(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()

	blobs := [][]byte{
		[]byte("entropyfs go binding: the quick brown fox jumps over the lazy dog"),
		make([]byte, 4096), // zeros
		bytes.Repeat([]byte{0xAB}, 8192),
		{},
	}
	ids := make([]BlobID, len(blobs))
	for i, b := range blobs {
		id, err := e.PutBlob(b)
		if err != nil {
			t.Fatalf("PutBlob %d: %v", i, err)
		}
		ids[i] = id
	}

	// Dedup identity: same bytes -> same id.
	id2, err := e.PutBlob(blobs[0])
	if err != nil {
		t.Fatalf("PutBlob dup: %v", err)
	}
	if id2 != ids[0] {
		t.Fatal("equal bytes must dedup to the same id")
	}

	for i, b := range blobs {
		got, err := e.GetBlob(ids[i])
		if err != nil {
			t.Fatalf("GetBlob %d: %v", i, err)
		}
		if !bytes.Equal(got, b) {
			t.Fatalf("GetBlob %d: byte mismatch (got %d bytes, want %d)", i, len(got), len(b))
		}
	}
}

func TestReadBlobRange(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()

	blob := make([]byte, 4096)
	for i := range blob {
		blob[i] = byte(i * 7)
	}
	id, err := e.PutBlob(blob)
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}

	// Exact window.
	got, err := e.ReadBlobRange(id, 100, 64)
	if err != nil {
		t.Fatalf("ReadBlobRange: %v", err)
	}
	if !bytes.Equal(got, blob[100:164]) {
		t.Fatal("range bytes must match exactly")
	}

	// EOF-clipped.
	got, err = e.ReadBlobRange(id, 4090, 100)
	if err != nil {
		t.Fatalf("ReadBlobRange clip: %v", err)
	}
	if !bytes.Equal(got, blob[4090:]) {
		t.Fatal("EOF-clipped range must match")
	}

	// Offset beyond EOF -> empty, no error.
	got, err = e.ReadBlobRange(id, 8192, 64)
	if err != nil {
		t.Fatalf("ReadBlobRange oob: %v", err)
	}
	if len(got) != 0 {
		t.Fatalf("out-of-bounds offset must return empty, got %d bytes", len(got))
	}

	// usize::MAX-equivalent length (EOF-clipped, not an error).
	got, err = e.ReadBlobRange(id, 0, int(^uint(0)>>1))
	if err != nil {
		t.Fatalf("ReadBlobRange full: %v", err)
	}
	if !bytes.Equal(got, blob) {
		t.Fatal("full-length range must return the whole blob")
	}
}

func TestContainsSyncCompactMetrics(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()

	id, err := e.PutBlob([]byte("maintenance blob"))
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}
	if ok, err := e.Contains(id); err != nil || !ok {
		t.Fatalf("Contains stored: ok=%v err=%v", ok, err)
	}
	if ok, err := e.Contains(BlobID{}); err != nil || ok {
		t.Fatalf("Contains unknown: ok=%v err=%v", ok, err)
	}

	if err := e.Sync(); err != nil {
		t.Fatalf("Sync: %v", err)
	}

	rep, err := e.Compact()
	if err != nil {
		t.Fatalf("Compact: %v", err)
	}
	if rep.PhysicalUsedAfterBytes == 0 {
		t.Fatal("compact must report physical bytes")
	}

	m, err := e.Metrics()
	if err != nil {
		t.Fatalf("Metrics: %v", err)
	}
	if m.SchemaVersion != 2 {
		t.Fatalf("metrics schema version = %d, want 2 (the 12C-1-3 pressure block)", m.SchemaVersion)
	}
	if m.Accounting.PhysicalUsedBytes == 0 {
		t.Fatal("metrics must report physical used bytes")
	}
	if len(m.Raw) == 0 {
		t.Fatal("metrics Raw must carry the native DTO")
	}
	// The 12C-1-3 pressure witness is parsed (cumulative fields may be
	// zero on a fresh engine; the block must exist and be coherent).
	if m.Pressure.Samples != 0 || m.Pressure.EnterEvents != 0 {
		t.Fatalf("fresh engine must report zero pressure witnesses, got %+v", m.Pressure)
	}
}

func TestCloseSemantics(t *testing.T) {
	e, _ := mustCreate(t)
	if err := e.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// Every operation after close -> ErrClosed.
	if _, err := e.PutBlob([]byte("x")); !errors.Is(err, ErrClosed) {
		t.Fatalf("PutBlob after close: %v", err)
	}
	if _, err := e.GetBlob(BlobID{}); !errors.Is(err, ErrClosed) {
		t.Fatalf("GetBlob after close: %v", err)
	}
	if _, err := e.ReadBlobRange(BlobID{}, 0, 1); !errors.Is(err, ErrClosed) {
		t.Fatalf("ReadBlobRange after close: %v", err)
	}
	if _, err := e.Contains(BlobID{}); !errors.Is(err, ErrClosed) {
		t.Fatalf("Contains after close: %v", err)
	}
	if err := e.Sync(); !errors.Is(err, ErrClosed) {
		t.Fatalf("Sync after close: %v", err)
	}
	if _, err := e.Compact(); !errors.Is(err, ErrClosed) {
		t.Fatalf("Compact after close: %v", err)
	}
	if _, err := e.Metrics(); !errors.Is(err, ErrClosed) {
		t.Fatalf("Metrics after close: %v", err)
	}
	// Double close -> ErrClosed (not a crash, no double free).
	if err := e.Close(); !errors.Is(err, ErrClosed) {
		t.Fatalf("double close: %v", err)
	}
}

func TestBlobSurvivesReopen(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "store")
	e, err := Create(dir, OpenOptions{})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	blob := bytes.Repeat([]byte("persist-me-"), 300)
	id, err := e.PutBlob(blob)
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}
	// The mount lock is exclusive: the first engine must close BEFORE a
	// second open of the same store (one reader OR writer at a time).
	if err := e.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	e, err = Open(dir, OpenOptions{})
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer e.Close()
	got, err := e.GetBlob(id)
	if err != nil {
		t.Fatalf("GetBlob after reopen: %v", err)
	}
	if !bytes.Equal(got, blob) {
		t.Fatal("blob must survive close/reopen byte-exact")
	}
}

func TestReadOnlyOpen(t *testing.T) {
	e, dir := mustCreate(t)
	blob := []byte("ro blob bytes that must stay readable")
	id, err := e.PutBlob(blob)
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}
	// Ack puts live in the mutation log; the RO open observes only the
	// last durable CHECKPOINT (replay is a write), so the blob must be
	// made durable before the RO reopen (documented 12E.3 semantics).
	if err := e.Sync(); err != nil {
		t.Fatalf("Sync: %v", err)
	}
	e.Close()

	ro, err := Open(dir, OpenOptions{ReadOnly: true})
	if err != nil {
		t.Fatalf("Open RO: %v", err)
	}
	defer ro.Close()
	if _, err := ro.PutBlob([]byte("nope")); !errors.Is(err, ErrUnsupported) {
		t.Fatalf("PutBlob on RO must be ErrUnsupported, got %v", err)
	}
	if err := ro.Sync(); !errors.Is(err, ErrUnsupported) {
		t.Fatalf("Sync on RO must be ErrUnsupported, got %v", err)
	}
	// Reads still work byte-exact.
	got, err := ro.GetBlob(id)
	if err != nil {
		t.Fatalf("GetBlob on RO: %v", err)
	}
	if !bytes.Equal(got, blob) {
		t.Fatal("RO read must be byte-exact")
	}
}
