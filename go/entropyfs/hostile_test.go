package entropyfs

import (
	"bytes"
	"errors"
	"math"
	"testing"
)

// The hostile-input court: every Go-provided length/offset/id is
// validated before conversion to native integer types. No conversion may
// silently wrap; the native resource limits remain authoritative.

func TestNegativeOffsetsAndLengths(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()
	id, err := e.PutBlob(bytes.Repeat([]byte{1}, 64))
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}

	if _, err := e.ReadBlobRange(id, -1, 8); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("negative offset: %v", err)
	}
	if _, err := e.ReadBlobRange(id, 0, -8); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("negative length: %v", err)
	}
}

func TestHugeLengthIsClippedNotWrapped(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()
	blob := bytes.Repeat([]byte{0x55}, 512)
	id, err := e.PutBlob(blob)
	if err != nil {
		t.Fatalf("PutBlob: %v", err)
	}
	// maxInt length must EOF-clip (pread semantics), never wrap/panic.
	got, err := e.ReadBlobRange(id, 0, math.MaxInt)
	if err != nil {
		t.Fatalf("maxInt length: %v", err)
	}
	if !bytes.Equal(got, blob) {
		t.Fatal("maxInt length must return the full blob")
	}
}

func TestNilAndEmptyBuffers(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()

	// Empty and nil puts are valid (an empty blob).
	id1, err := e.PutBlob(nil)
	if err != nil {
		t.Fatalf("PutBlob(nil): %v", err)
	}
	id2, err := e.PutBlob([]byte{})
	if err != nil {
		t.Fatalf("PutBlob(empty): %v", err)
	}
	if id1 != id2 {
		t.Fatal("nil and empty must dedup to the same id")
	}
	got, err := e.GetBlob(id1)
	if err != nil {
		t.Fatalf("GetBlob(empty): %v", err)
	}
	if len(got) != 0 {
		t.Fatalf("empty blob must read back empty, got %d bytes", len(got))
	}
}

func TestMalformedIdIsNotFound(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()

	var zero BlobID
	if _, err := e.GetBlob(zero); !errors.Is(err, ErrNotFound) {
		t.Fatalf("zero id: %v", err)
	}
	if _, err := e.ReadBlobRange(zero, 0, 4); !errors.Is(err, ErrNotFound) {
		t.Fatalf("zero id range: %v", err)
	}
	if ok, err := e.Contains(zero); err != nil || ok {
		t.Fatalf("zero id contains: ok=%v err=%v", ok, err)
	}
}

func TestEmptyPathIsInvalidArgument(t *testing.T) {
	if _, err := Create("", OpenOptions{}); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("empty path: %v", err)
	}
	if _, err := Open("", OpenOptions{}); !errors.Is(err, ErrInvalidArgument) {
		t.Fatalf("empty path open: %v", err)
	}
}

func TestErrorIsSentinelMatches(t *testing.T) {
	e, _ := mustCreate(t)
	defer e.Close()
	if _, err := e.GetBlob(BlobID{}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("errors.Is(ErrNotFound): %v", err)
	}
	// The sentinel itself must satisfy Is against itself.
	if !errors.Is(ErrNotFound, ErrNotFound) {
		t.Fatal("sentinel identity")
	}
}
