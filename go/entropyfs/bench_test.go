package entropyfs

import (
	"bytes"
	"fmt"
	"testing"
)

// The FFI-overhead bench: measures the Go-binding op costs so the
// Rust-vs-C-vs-Go comparison is measured, not guessed. The direct-Rust
// reference numbers come from the sealed 12E.13 adoption court
// (`evidence/performance/adoption-oracle-*/`): put 33–138 MiB/s,
// get 114–691 MiB/s depending on workload. These benches are
// informational (not a gate); run with `-bench . -benchmem`.

func benchPut(b *testing.B, size int) {
	e, _ := mustCreateB(b)
	defer e.Close()
	// Distinct content per iteration: a repeated identical put would hit
	// the fast-dedup path (near-free). This bench measures the real
	// write path (foreground search + commit), which is what the sealed
	// adoption court (12E.13) measured at 33–138 MiB/s for distinct
	// blobs.
	blob := bytes.Repeat([]byte{0x5A}, size)
	b.SetBytes(int64(size))
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		blob[0] = byte(i)
		blob[1] = byte(i >> 8)
		if _, err := e.PutBlob(blob); err != nil {
			b.Fatalf("PutBlob: %v", err)
		}
	}
}

func mustCreateB(b *testing.B) (*Engine, string) {
	b.Helper()
	dir := fmt.Sprintf("%s/bench-store", b.TempDir())
	e, err := Create(dir, OpenOptions{})
	if err != nil {
		b.Fatalf("Create: %v", err)
	}
	return e, dir
}

func BenchmarkPutBlob64KiB(b *testing.B) { benchPut(b, 64*1024) }
func BenchmarkPutBlob1MiB(b *testing.B)  { benchPut(b, 1024*1024) }

func BenchmarkGetBlob64KiB(b *testing.B) {
	e, _ := mustCreateB(b)
	defer e.Close()
	blob := bytes.Repeat([]byte{0xA5}, 64*1024)
	id, err := e.PutBlob(blob)
	if err != nil {
		b.Fatalf("PutBlob: %v", err)
	}
	b.SetBytes(int64(len(blob)))
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		got, err := e.GetBlob(id)
		if err != nil {
			b.Fatalf("GetBlob: %v", err)
		}
		if !bytes.Equal(got, blob) {
			b.Fatal("mismatch")
		}
	}
}

func BenchmarkReadBlobRange4KiB(b *testing.B) {
	e, _ := mustCreateB(b)
	defer e.Close()
	blob := bytes.Repeat([]byte{0x3C}, 1024*1024)
	id, err := e.PutBlob(blob)
	if err != nil {
		b.Fatalf("PutBlob: %v", err)
	}
	b.SetBytes(4096)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, err := e.ReadBlobRange(id, 0, 4096); err != nil {
			b.Fatalf("ReadBlobRange: %v", err)
		}
	}
}

func BenchmarkSync(b *testing.B) {
	e, _ := mustCreateB(b)
	defer e.Close()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if err := e.Sync(); err != nil {
			b.Fatalf("Sync: %v", err)
		}
	}
}
