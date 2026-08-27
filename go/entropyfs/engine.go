// Package entropyfs is the Go binding for the EntropyFS embeddable
// immutable-object engine (Phase 12E.15).
//
// # Architecture
//
// The binding is a THIN cgo adapter over the stable C ABI
// (include/entropyfs.h, src/ffi): it does not reimplement anything. The
// dependency direction is:
//
//	EntropyFS Engine API
//		-> stable C ABI
//		-> Go cgo binding
//
// There is deliberately NO bespoke Rust<->Go path: content identity,
// durability, errors, resource bounds, compaction and corruption
// handling are exactly the C ABI's — which are exactly the Rust Engine
// API's.
//
// # Handle model
//
// An *Engine wraps ONE opaque native handle. Native handles are never
// exposed; Rust layout never crosses into Go. The handle is safe for
// concurrent use by many goroutines (the native engine's contract: many
// concurrent readers + writers). Close is the lifecycle barrier: it is
// exclusive against every in-flight operation (an RWMutex — operations
// take the read lock and therefore run CONCURRENTLY with each other;
// Close takes the write lock). After Close returns, every operation
// fails with ErrClosed; the native handle is released exactly once.
//
// # Ownership
//
// For every native-allocated buffer (GetBlob, ReadBlobRange, Metrics),
// the binding copies the bytes into a Go []byte and then releases the
// native buffer through the C ABI's single free mechanism. There is no
// zero-copy exposure of native memory.
//
// # Durability
//
// PutBlob acknowledges at the mutation log (process-crash-safe,
// visible to later opens; NOT power-durable). Sync() is the durability
// boundary: after it returns, every acknowledged put survives power
// loss (the native group-commit machinery). The binding invents no
// durability semantics of its own.
//
// # Errors
//
// The C ABI's stable machine-readable classes map to ErrorCode; errors.Is
// works against the package sentinels (ErrNotFound, ErrCorruptStore, ...).
// Diagnostic messages may improve over time; programs must not parse
// them.
package entropyfs

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -lentropyfs

#include <entropyfs.h>
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// BlobID is the 32-byte BLAKE3 content id of a blob's logical bytes.
// Equal bytes always produce equal ids; ids are stable across
// compaction, representation migration, and encoder-policy changes.
type BlobID [32]byte

// OpenOptions are the stable public open options. ReadOnly opens an
// existing store read-only (unknown ro_compat bits are permitted; every
// write fails with ErrUnsupported).
type OpenOptions struct {
	ReadOnly bool
}

// CompactionReport is the stable public compaction result. ReclaimedBytes
// and PhysicalUsedAfterBytes come from the C ABI; the remaining fields
// are populated from the metrics surface when available.
type CompactionReport struct {
	UnreachableBeforeBytes uint64
	ReclaimedBytes         uint64
	UnreachableAfterBytes  uint64
	PhysicalUsedAfterBytes uint64
	LiveBytesAfter         uint64
}

// AccountingMetrics is the byte-accounting slice of Metrics.
type AccountingMetrics struct {
	LogicalBytes          uint64
	ReachableBytes        uint64
	PhysicalUsedBytes     uint64
	PhysicalCapacityBytes uint64
	PhysicalFreeBytes     uint64
	ObjectCount           uint64
	DataRecordCount       uint64
	BlobCount             uint64
}

// Metrics is the versioned operational metrics snapshot. Raw carries the
// complete native JSON DTO (the same schema as `entropyfs metrics --json`);
// the typed fields are the stable, documented subset.
type Metrics struct {
	SchemaVersion uint32
	Accounting    AccountingMetrics
	Raw           []byte
}

// Engine is an opaque handle to one native engine (one store). Safe for
// concurrent use by many goroutines; see the package doc for Close.
type Engine struct {
	mu     sync.RWMutex // read lock: concurrent ops; write lock: Close
	handle *C.entropyfs_engine
	closed bool
}

// String renders a BlobID as 64 lowercase hex characters.
func (id BlobID) String() string {
	const hex = "0123456789abcdef"
	b := make([]byte, 64)
	for i, v := range id {
		b[i*2] = hex[v>>4]
		b[i*2+1] = hex[v&0xf]
	}
	return string(b)
}

// lastErrDetail fetches the calling thread's native last-error detail.
func lastErrDetail() string {
	buf := make([]byte, 512)
	C.entropyfs_last_error((*C.char)(unsafe.Pointer(&buf[0])), C.size_t(len(buf)))
	if i := indexByte(buf, 0); i >= 0 {
		buf = buf[:i]
	}
	return string(buf)
}

func indexByte(b []byte, c byte) int {
	for i, v := range b {
		if v == c {
			return i
		}
	}
	return -1
}

// wrap maps a native return class to a Go error (nil for EFS_OK).
func wrap(rc C.int) error {
	code := ErrorCode(uint32(rc))
	if code == CodeOK {
		return nil
	}
	return &Error{Code: code, Message: lastErrDetail()}
}

// Create makes a fresh store at path and returns the owning Engine.
func Create(path string, opts OpenOptions) (*Engine, error) {
	return open(path, opts, C.EFS_ENGINE_CREATE)
}

// Open opens an existing store at path (read-only when opts.ReadOnly).
func Open(path string, opts OpenOptions) (*Engine, error) {
	mode := C.int(C.EFS_ENGINE_OPEN)
	if opts.ReadOnly {
		mode = C.int(C.EFS_ENGINE_OPEN_RO)
	}
	return open(path, opts, mode)
}

func open(path string, _ OpenOptions, mode C.int) (*Engine, error) {
	if path == "" {
		return nil, ErrInvalidArgument
	}
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	var h *C.entropyfs_engine
	rc := C.entropyfs_engine_open(cpath, mode, &h)
	if err := wrap(rc); err != nil {
		return nil, err
	}
	e := &Engine{handle: h}
	// Keep the native engine pinned to this goroutine's OS thread is NOT
	// required (the handle is Send+Sync); but pinning the finalizer to a
	// thread avoids finalizer-vs-close races on the handle pointer.
	runtime.SetFinalizer(e, (*Engine).finalize)
	return e, nil
}

func (e *Engine) finalize() {
	// Best-effort release if the caller never closed. This is the
	// double-close guard: Close marks closed BEFORE calling the native
	// close, so a finalizer after Close is a no-op.
	e.mu.Lock()
	defer e.mu.Unlock()
	if !e.closed {
		e.closed = true
		C.entropyfs_engine_close(e.handle)
	}
}

// Close releases the native handle. Exclusive against in-flight
// operations; after Close returns, every operation fails with ErrClosed.
func (e *Engine) Close() error {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.closed {
		return ErrClosed
	}
	e.closed = true
	if err := wrap(C.entropyfs_engine_close(e.handle)); err != nil {
		return err
	}
	runtime.SetFinalizer(e, nil)
	return nil
}

// checkOpen verifies the handle is usable (read-locked by the caller).
func (e *Engine) checkOpen() error {
	if e.closed {
		return ErrClosed
	}
	return nil
}

// PutBlob stores data (Ack durability; power-durable after Sync) and
// returns its BlobID. Equal bytes always return the same id (dedup).
func (e *Engine) PutBlob(data []byte) (BlobID, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return BlobID{}, err
	}
	var id BlobID
	var p *C.uint8_t
	if len(data) > 0 {
		p = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	rc := C.entropyfs_blob_put(e.handle, p, C.size_t(len(data)), (*C.uint8_t)(unsafe.Pointer(&id[0])))
	if err := wrap(rc); err != nil {
		return BlobID{}, err
	}
	return id, nil
}

// GetBlob fetches a blob's complete bytes, byte-exact (the native hash
// gate verifies the materialized bytes hash to the id).
func (e *Engine) GetBlob(id BlobID) ([]byte, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return nil, err
	}
	var buf *C.uint8_t
	var n C.size_t
	rc := C.entropyfs_blob_get(e.handle, (*C.uint8_t)(unsafe.Pointer(&id[0])), &buf, &n)
	if err := wrap(rc); err != nil {
		return nil, err
	}
	defer C.entropyfs_free(buf)
	return C.GoBytes(unsafe.Pointer(buf), C.int(n)), nil
}

// ReadBlobRange reads a byte range of a blob (EOF-clipped like pread;
// length <= 0 reads nothing, offset >= blob size returns empty).
func (e *Engine) ReadBlobRange(id BlobID, offset int64, length int) ([]byte, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return nil, err
	}
	if offset < 0 {
		return nil, fmt.Errorf("%w: negative offset %d", ErrInvalidArgument, offset)
	}
	if length < 0 {
		return nil, fmt.Errorf("%w: negative length %d", ErrInvalidArgument, length)
	}
	var buf *C.uint8_t
	var n C.size_t
	rc := C.entropyfs_blob_read_range(
		e.handle,
		(*C.uint8_t)(unsafe.Pointer(&id[0])),
		C.uint64_t(offset),
		C.size_t(length),
		&buf,
		&n,
	)
	if err := wrap(rc); err != nil {
		return nil, err
	}
	defer C.entropyfs_free(buf)
	return C.GoBytes(unsafe.Pointer(buf), C.int(n)), nil
}

// Contains reports whether a blob id exists (was put and acknowledged).
func (e *Engine) Contains(id BlobID) (bool, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return false, err
	}
	var present C.int
	rc := C.entropyfs_contains(e.handle, (*C.uint8_t)(unsafe.Pointer(&id[0])), &present)
	if err := wrap(rc); err != nil {
		return false, err
	}
	return present != 0, nil
}

// Sync makes all acknowledged puts power-durable (the durability
// boundary; the native group-commit machinery).
func (e *Engine) Sync() error {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return err
	}
	return wrap(C.entropyfs_sync(e.handle))
}

// Compact reclaims unreachable bytes and returns the stable report.
func (e *Engine) Compact() (CompactionReport, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return CompactionReport{}, err
	}
	var reclaimed, physical C.uint64_t
	rc := C.entropyfs_compact(e.handle, &reclaimed, &physical)
	if err := wrap(rc); err != nil {
		return CompactionReport{}, err
	}
	return CompactionReport{
		ReclaimedBytes:         uint64(reclaimed),
		PhysicalUsedAfterBytes: uint64(physical),
	}, nil
}

// Metrics returns the versioned operational metrics snapshot (the native
// JSON DTO parsed into the stable typed subset plus Raw).
func (e *Engine) Metrics() (Metrics, error) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if err := e.checkOpen(); err != nil {
		return Metrics{}, err
	}
	var buf *C.uint8_t
	var n C.size_t
	rc := C.entropyfs_metrics_json(e.handle, &buf, &n)
	if err := wrap(rc); err != nil {
		return Metrics{}, err
	}
	defer C.entropyfs_free(buf)
	raw := C.GoBytes(unsafe.Pointer(buf), C.int(n))
	return parseMetrics(raw)
}

var _ = errors.Is // keep the errors import when sentinels move
