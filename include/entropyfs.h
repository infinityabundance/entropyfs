/*
 * entropyfs.h — Phase 12E.14: the stable C ABI (opaque-handle engine facade).
 *
 * PURPOSE
 *
 * The embeddable immutable-object engine (content-addressed blobs over
 * the persistent store) exposed to C and every language that links C.
 * The handle is opaque: no Rust layout, no internal structs, no store
 * types ever cross this boundary.
 *
 * ABI VERSIONING
 *
 * EFS_ABI_VERSION is INDEPENDENT of the on-disk format version. They are
 * separate compatibility domains: a program compiled against ABI 1 can
 * open stores of any supported format major. Query
 * entropyfs_abi_version() at runtime and fail gracefully on mismatch.
 *
 * OWNERSHIP RULES (normative)
 *
 * 1. Handles. entropyfs_engine_open writes the opaque handle into the
 *    out-param. The caller OWNS it; entropyfs_engine_close CONSUMES it.
 *    Close each handle exactly once; using a handle after close is
 *    undefined behavior. The handle is safe to SHARE across threads for
 *    concurrent operations (many concurrent readers + writers; close
 *    drains in-flight operations).
 * 2. Caller-owned inputs. data/len (put) and id (32 bytes) are borrowed
 *    for the call's duration; the callee never retains them.
 * 3. Callee-allocated outputs. entropyfs_blob_get / _read_range /
 *    metrics_json return a pointer the callee allocated. The caller OWNS
 *    it and MUST release it with entropyfs_free — the ONE release
 *    mechanism. Never free it any other way; never free it twice.
 * 4. Errors. Every function returns the stable numeric class
 *    (EFS_OK = 0). entropyfs_last_error fetches the thread-local
 *    human-readable detail — diagnostic only, never parsed by programs.
 *
 * CONCURRENCY
 *
 * The handle is thread-safe for concurrent use (the Engine contract).
 * entropyfs_last_error is per-thread.
 */
#ifndef ENTROPYFS_H
#define ENTROPYFS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The C ABI version (independent of the on-disk format version). */
#define EFS_ABI_VERSION 1u

/* Open modes for entropyfs_engine_open. */
#define EFS_ENGINE_OPEN 0    /* open an existing store */
#define EFS_ENGINE_CREATE 1  /* create a fresh store */
#define EFS_ENGINE_OPEN_RO 2 /* open an existing store read-only (writes -> EFS_UNSUPPORTED) */

/* Stable error classes (the machine-readable contract). Programs switch
 * on these; never parse entropyfs_last_error strings. */
enum entropyfs_error {
    EFS_OK = 0,
    EFS_NOT_FOUND = 1,
    EFS_INVALID_ARGUMENT = 2,
    EFS_CORRUPT_STORE = 3,
    EFS_INCOMPATIBLE_FORMAT = 4,
    EFS_RESOURCE_LIMIT = 5,
    EFS_IO = 6,
    EFS_BUSY = 7,
    EFS_UNSUPPORTED = 8,
    EFS_INTERNAL = 9,
    EFS_CLOSED = 10,
};

/* Opaque engine handle. */
typedef struct entropyfs_engine entropyfs_engine;

/* The content id: 32 bytes (BLAKE3 of the logical bytes). Equal bytes
 * always produce equal ids; ids are stable across compaction,
 * representation migration, and encoder-policy changes. */
#define EFS_BLOB_ID_LEN 32

/* --- version --------------------------------------------------------- */

/* The current C ABI version. */
uint32_t entropyfs_abi_version(void);

/* --- lifecycle ------------------------------------------------------- */

/* Open (mode EFS_ENGINE_OPEN) or create (mode EFS_ENGINE_CREATE) an
 * engine at path. On success writes the owned handle into *out_handle.
 * Returns the error class (EFS_OK = 0). */
int entropyfs_engine_open(const char *path, int mode,
                          entropyfs_engine **out_handle);

/* Close an engine, CONSUMING the handle. Close exactly once; using the
 * handle afterwards is undefined behavior. */
int entropyfs_engine_close(entropyfs_engine *handle);

/* --- blobs ---------------------------------------------------------- */

/* Put a blob (Ack durability: process-crash-safe; power-durable after
 * entropyfs_sync). id must point to EFS_BLOB_ID_LEN bytes; the content
 * id is written there. data is borrowed for the call. */
int entropyfs_blob_put(entropyfs_engine *handle, const uint8_t *data,
                       size_t len, uint8_t *id /* EFS_BLOB_ID_LEN out */);

/* Fetch a blob's complete bytes (byte-exact; the engine verifies the
 * returned bytes hash to the id). The callee allocates *out_buf — the
 * caller owns it and MUST release it with entropyfs_free. */
int entropyfs_blob_get(entropyfs_engine *handle,
                       const uint8_t *id /* EFS_BLOB_ID_LEN */,
                       uint8_t **out_buf, size_t *out_len);

/* Read a byte range of a blob (EOF-clipped like pread; len ==
 * SIZE_MAX reads to the end). Callee-allocated output — free with
 * entropyfs_free. */
int entropyfs_blob_read_range(entropyfs_engine *handle,
                              const uint8_t *id /* EFS_BLOB_ID_LEN */,
                              uint64_t offset, size_t len, uint8_t **out_buf,
                              size_t *out_len);

/* Whether a blob id exists (was put and acknowledged). *out = 1 or 0. */
int entropyfs_contains(entropyfs_engine *handle,
                       const uint8_t *id /* EFS_BLOB_ID_LEN */, int *out);

/* --- maintenance ----------------------------------------------------- */

/* Make all acknowledged puts power-durable (the durability boundary). */
int entropyfs_sync(entropyfs_engine *handle);

/* Compact (reclaim unreachable bytes). out_reclaimed / out_physical are
 * nullable. */
int entropyfs_compact(entropyfs_engine *handle, uint64_t *out_reclaimed,
                      uint64_t *out_physical);

/* Fetch the engine metrics as a JSON string (the versioned
 * EngineMetrics DTO; same schema as `entropyfs metrics --json`).
 * Callee-allocated — free with entropyfs_free. */
int entropyfs_metrics_json(entropyfs_engine *handle, uint8_t **out_buf,
                           size_t *out_len);

/* --- diagnostics ----------------------------------------------------- */

/* Fetch the calling thread's last error detail (truncated to cap bytes,
 * NUL-terminated when cap > 0). Returns 0 if the last call succeeded,
 * nonzero if it failed. Diagnostic only — never parse this string. */
int entropyfs_last_error(char *buf, size_t cap);

/* Release a pointer previously returned by the callee-allocating
 * functions. The ONE release mechanism; free exactly once. */
void entropyfs_free(uint8_t *ptr);

#ifdef __cplusplus
}
#endif

#endif /* ENTROPYFS_H */
