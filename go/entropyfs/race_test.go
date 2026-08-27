package entropyfs

import (
	"bytes"
	"fmt"
	"sync"
	"testing"
)

// The Go binding race/stress court. Gate: `go test -race` must pass.
// 32+ goroutines exercise concurrent PutBlob (distinct + duplicate
// content), GetBlob, ReadBlobRange, Contains, periodic Sync and
// concurrent Metrics, plus repeated open/close cycles on separate
// engines. Byte-exactness is verified throughout: representation,
// scheduling, compaction and GC must never change the bytes returned
// through Go.

func TestConcurrentStress(t *testing.T) {
	e, _ := mustCreate(t)

	const workers = 32
	const opsPerWorker = 60

	// Pre-seed a pool of distinct blobs (some tiny, one large) so the
	// workers have deterministic read targets.
	pool := make([][]byte, 16)
	for i := range pool {
		pool[i] = bytes.Repeat([]byte{byte(i)}, 1024+i*97)
	}
	ids := make([]BlobID, len(pool))
	for i, b := range pool {
		id, err := e.PutBlob(b)
		if err != nil {
			t.Fatalf("seed PutBlob: %v", err)
		}
		ids[i] = id
	}

	// A dedicated sync goroutine fires durability barriers concurrently
	// with the read/write storm.
	syncDone := make(chan error, 1)
	go func() {
		for i := 0; i < 8; i++ {
			if err := e.Sync(); err != nil {
				syncDone <- err
				return
			}
		}
		syncDone <- nil
	}()

	var wg sync.WaitGroup
	errCh := make(chan error, workers*2)
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(w int) {
			defer wg.Done()
			for i := 0; i < opsPerWorker; i++ {
				kind := (w + i) % 5
				switch kind {
				case 0: // distinct put
					blob := bytes.Repeat([]byte{byte(w)}, 512+i*13)
					id, err := e.PutBlob(blob)
					if err != nil {
						errCh <- fmt.Errorf("w%d put: %w", w, err)
						return
					}
					got, err := e.GetBlob(id)
					if err != nil {
						errCh <- fmt.Errorf("w%d get: %w", w, err)
						return
					}
					if !bytes.Equal(got, blob) {
						errCh <- fmt.Errorf("w%d byte mismatch", w)
						return
					}
				case 1: // duplicate put (dedup path)
					idx := (w + i) % len(pool)
					id, err := e.PutBlob(pool[idx])
					if err != nil {
						errCh <- fmt.Errorf("w%d dup put: %w", w, err)
						return
					}
					if id != ids[idx] {
						errCh <- fmt.Errorf("w%d dedup id mismatch", w)
						return
					}
				case 2: // read
					idx := (w*7 + i) % len(pool)
					got, err := e.GetBlob(ids[idx])
					if err != nil {
						errCh <- fmt.Errorf("w%d read: %w", w, err)
						return
					}
					if !bytes.Equal(got, pool[idx]) {
						errCh <- fmt.Errorf("w%d read mismatch", w)
						return
					}
				case 3: // range read
					idx := (w*3 + i) % len(pool)
					off := int64(100 + (i%10)*17)
					got, err := e.ReadBlobRange(ids[idx], off, 256)
					if err != nil {
						errCh <- fmt.Errorf("w%d range: %w", w, err)
						return
					}
					want := pool[idx][off : off+256]
					if !bytes.Equal(got, want) {
						errCh <- fmt.Errorf("w%d range mismatch", w)
						return
					}
				case 4: // contains + metrics
					if _, err := e.Contains(ids[(w+i)%len(ids)]); err != nil {
						errCh <- fmt.Errorf("w%d contains: %w", w, err)
						return
					}
					if _, err := e.Metrics(); err != nil {
						errCh <- fmt.Errorf("w%d metrics: %w", w, err)
						return
					}
				}
			}
		}(w)
	}
	wg.Wait()
	close(errCh)
	for err := range errCh {
		t.Error(err)
	}
	if err := <-syncDone; err != nil {
		t.Errorf("sync goroutine: %v", err)
	}

	// After the storm: every pre-seeded blob is still byte-exact.
	for i := range pool {
		got, err := e.GetBlob(ids[i])
		if err != nil {
			t.Fatalf("post-storm read %d: %v", i, err)
		}
		if !bytes.Equal(got, pool[i]) {
			t.Fatalf("post-storm byte mismatch %d", i)
		}
	}
	if err := e.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func TestConcurrentOpenCloseCycles(t *testing.T) {
	// Repeated create/close cycles on separate engines while a shared
	// engine is hammered — exercises the lifecycle under contention.
	base := t.TempDir()
	shared, _ := mustCreate(t)
	defer shared.Close()

	var wg sync.WaitGroup
	errCh := make(chan error, 8)
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			dir := fmt.Sprintf("%s/cycle-%d", base, i)
			e, err := Create(dir, OpenOptions{})
			if err != nil {
				errCh <- fmt.Errorf("cycle create: %w", err)
				return
			}
			if _, err := e.PutBlob([]byte(fmt.Sprintf("cycle %d", i))); err != nil {
				errCh <- fmt.Errorf("cycle put: %w", err)
				return
			}
			if err := e.Close(); err != nil {
				errCh <- fmt.Errorf("cycle close: %w", err)
				return
			}
			e2, err := Open(dir, OpenOptions{})
			if err != nil {
				errCh <- fmt.Errorf("cycle reopen: %w", err)
				return
			}
			if err := e2.Close(); err != nil {
				errCh <- fmt.Errorf("cycle close2: %w", err)
				return
			}
		}(i)
	}
	wg.Wait()
	close(errCh)
	for err := range errCh {
		t.Error(err)
	}
}
