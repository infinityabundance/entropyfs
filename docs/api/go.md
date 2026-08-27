# The Go binding (Phase 12E.15)

The Go binding is a THIN cgo adapter over the stable C ABI — it is not a
second storage API. Dependency direction:

```text
EntropyFS Engine API
    -> stable C ABI (include/entropyfs.h, ABI v1)
    -> Go cgo binding (go/entropyfs)
```

Content identity, durability, errors, resource bounds, compaction and
corruption handling are EXACTLY the C ABI's — which are exactly the Rust
Engine API's. There is no Go-only storage semantics: no Go content ids,
no Go metadata, no Go-side representation selection.

## Installation

```sh
cargo build --release                 # builds target/release/libentropyfs.so
cd go
CGO_LDFLAGS="-L../target/release -lentropyfs" \
LD_LIBRARY_PATH="../target/release" go test ./...
```

The module is `github.com/infinityabundance/entropyfs/go`. The native
library is a required runtime dependency (this is cgo; the binding is
not pure Go). `tools/go-test.sh` runs the full court (vet, tests, the
mandatory `-race` gate, benchmarks).

## API

```go
type BlobID [32]byte              // BLAKE3 content id; String() = 64 hex
type OpenOptions struct { ReadOnly bool }
type Engine struct { /* opaque native handle */ }

func Create(path string, opts OpenOptions) (*Engine, error)
func Open(path string, opts OpenOptions) (*Engine, error)

func (e *Engine) PutBlob(data []byte) (BlobID, error)
func (e *Engine) GetBlob(id BlobID) ([]byte, error)
func (e *Engine) ReadBlobRange(id BlobID, offset int64, length int) ([]byte, error)
func (e *Engine) Contains(id BlobID) (bool, error)
func (e *Engine) Sync() error
func (e *Engine) Compact() (CompactionReport, error)
func (e *Engine) Metrics() (Metrics, error)
func (e *Engine) Close() error
```

## Thread/goroutine safety

One `*Engine` is safe for simultaneous use by many goroutines (many
concurrent readers + writers — the native engine's contract). The
binding holds a per-engine RWMutex: operations take the READ lock and
therefore run CONCURRENTLY with each other; `Close` takes the WRITE
lock, so it is exclusive against every in-flight operation. After
`Close` returns, every operation fails with `ErrClosed` and the native
handle is released exactly once (a finalizer is the double-close guard).

Note the native store's exclusive mount lock: only ONE engine may hold a
given store open at a time (one reader OR writer per store).

## Ownership

Every native-allocated buffer (GetBlob / ReadBlobRange / Metrics) is
copied into a Go `[]byte` and the native buffer is then released through
the C ABI's single free mechanism. There is no zero-copy exposure of
native memory; no Go pointer is ever retained by the native side.

## Durability

- `PutBlob` acknowledges at the mutation log: process-crash-safe and
  visible to later opens, NOT power-durable.
- `Sync()` is the durability boundary: after it returns, every
  acknowledged put survives power loss (the native group-commit
  generations, Phase 12B). The binding invents no durability semantics.

## Errors

`ErrorCode` mirrors the C ABI classes; `*Error{Code, Message}` supports
`errors.Is` against the package sentinels:

```go
errors.Is(err, entropyfs.ErrNotFound)
errors.Is(err, entropyfs.ErrCorruptStore)
```

Diagnostic messages may improve over time; programs must never parse
them.

## Resource bounds

Offsets/lengths are validated before conversion to native integer types
(negative offset/length -> `ErrInvalidArgument`; oversized lengths
EOF-clip like `pread`, never wrap). The native resource limits remain
authoritative. The hostile-input court (`hostile_test.go`) pins this.

## Supported platforms

linux/amd64 (the Phase 12E.8 distribution matrix: Almalinux 10.2,
Ubuntu Server 26.04, openSUSE Leap 16 — see
`docs/portability/support-matrix.md`). Requires cgo, a C toolchain, and
the native `libentropyfs.so` at runtime.

## Version compatibility

Three independent domains: the Go package version, the C ABI version
(`entropyfs_abi_version()`, currently 1), and the on-disk format
version. They evolve independently; check the ABI version at runtime.

## Example

`go/examples/content-store/` is a deliberately thin concurrent
content-addressed HTTP service (PUT /blob, GET /blob/{id} with optional
range, GET /metrics) proving the engine can be embedded without FUSE.

## Courts

`tools/go-test.sh` runs: `go vet`, the correctness/exactness court
(byte-for-byte Rust=C=Go semantics), the hostile-input court, the
32-goroutine race/stress court under `go test -race` (mandatory), the
example smoke, and the FFI-overhead benchmarks. The enterprise
portability lane (the three mandatory distros) runs the same binding
court inside the Phase 12E.8 Docker-VM court.
