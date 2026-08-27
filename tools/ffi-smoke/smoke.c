/*
 * Phase 12E.14 C-ABI smoke test — the C caller's view of the facade.
 *
 * Exercises: ABI version query; create/open; put (distinct + duplicate
 * dedup identity); get (byte-exact); range read (byte-exact); contains;
 * sync; compact; metrics JSON (free contract); the classified error path
 * (missing blob -> EFS_NOT_FOUND with last_error detail); the
 * open/close lifecycle. Every callee-allocated buffer is released with
 * entropyfs_free exactly once.
 *
 * Build/run: tools/ffi-smoke.sh (compile + link against the cdylib).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "entropyfs.h"

static int failures = 0;

static void check(int cond, const char *what) {
    if (cond) {
        printf("  ok: %s\n", what);
    } else {
        printf("  FAIL: %s\n", what);
        failures++;
    }
}

static const char *last_err(void) {
    static char buf[512];
    entropyfs_last_error(buf, sizeof buf);
    return buf;
}

/* portable substring search (avoids the _GNU_SOURCE memmem dependency). */
static int contains(const uint8_t *hay, size_t n, const char *needle) {
    size_t m = strlen(needle);
    if (m > n) return 0;
    for (size_t i = 0; i + m <= n; i++) {
        if (memcmp(hay + i, needle, m) == 0) return 1;
    }
    return 0;
}

int main(void) {
    printf("== entropyfs C ABI smoke ==\n");

    check(entropyfs_abi_version() == EFS_ABI_VERSION, "abi version matches header");

    if (system("rm -rf /tmp/efs-smoke-store") != 0) { /* best effort */ }

    /* create */
    entropyfs_engine *h = NULL;
    int rc = entropyfs_engine_open("/tmp/efs-smoke-store", EFS_ENGINE_CREATE, &h);
    check(rc == EFS_OK && h != NULL, "create");
    if (rc != EFS_OK || !h) {
        printf("  fatal: %s\n", last_err());
        return 1;
    }

    /* put: three blobs + one duplicate */
    uint8_t id_a[EFS_BLOB_ID_LEN], id_b[EFS_BLOB_ID_LEN], id_c[EFS_BLOB_ID_LEN];
    uint8_t id_a2[EFS_BLOB_ID_LEN];
    const char *blob_a = "entropyfs c-abi smoke: the quick brown fox jumps over the lazy dog";
    uint8_t blob_b[4096];
    for (size_t i = 0; i < sizeof blob_b; i++) blob_b[i] = (uint8_t)(i * 31);
    const char *blob_c = "{\"schema\":\"smoke\",\"ok\":true}";

    rc = entropyfs_blob_put(h, (const uint8_t *)blob_a, strlen(blob_a), id_a);
    check(rc == EFS_OK, "put a");
    rc = entropyfs_blob_put(h, blob_b, sizeof blob_b, id_b);
    check(rc == EFS_OK, "put b");
    rc = entropyfs_blob_put(h, (const uint8_t *)blob_c, strlen(blob_c), id_c);
    check(rc == EFS_OK, "put c");
    rc = entropyfs_blob_put(h, (const uint8_t *)blob_a, strlen(blob_a), id_a2);
    check(rc == EFS_OK && memcmp(id_a, id_a2, EFS_BLOB_ID_LEN) == 0,
          "duplicate put dedups to the same id");

    /* get: byte-exact */
    uint8_t *buf = NULL;
    size_t len = 0;
    rc = entropyfs_blob_get(h, id_a, &buf, &len);
    check(rc == EFS_OK && len == strlen(blob_a) && memcmp(buf, blob_a, len) == 0,
          "get a byte-exact");
    entropyfs_free(buf);
    buf = NULL;

    rc = entropyfs_blob_get(h, id_b, &buf, &len);
    check(rc == EFS_OK && len == sizeof blob_b && memcmp(buf, blob_b, len) == 0,
          "get b byte-exact");
    entropyfs_free(buf);
    buf = NULL;

    /* range read: 64 bytes at offset 100 of blob b */
    rc = entropyfs_blob_read_range(h, id_b, 100, 64, &buf, &len);
    check(rc == EFS_OK && len == 64 && memcmp(buf, blob_b + 100, 64) == 0,
          "range read byte-exact");
    entropyfs_free(buf);
    buf = NULL;

    /* contains */
    int present = 0;
    rc = entropyfs_contains(h, id_a, &present);
    check(rc == EFS_OK && present == 1, "contains true for stored id");
    uint8_t junk[EFS_BLOB_ID_LEN];
    memset(junk, 0xEE, sizeof junk);
    rc = entropyfs_contains(h, junk, &present);
    check(rc == EFS_OK && present == 0, "contains false for unknown id");

    /* durability + maintenance */
    rc = entropyfs_sync(h);
    check(rc == EFS_OK, "sync");
    uint64_t reclaimed = 0, physical = 0;
    rc = entropyfs_compact(h, &reclaimed, &physical);
    check(rc == EFS_OK && physical > 0, "compact reports physical bytes");

    /* metrics JSON (free contract) */
    rc = entropyfs_metrics_json(h, &buf, &len);
    check(rc == EFS_OK && len > 0 && buf != NULL, "metrics JSON allocated");
    if (rc == EFS_OK && buf) {
        check(contains(buf, len, "\"schema_version\""),
              "metrics JSON carries the versioned schema");
        entropyfs_free(buf);
        buf = NULL;
    }

    /* classified error path: missing blob -> NOT_FOUND + detail */
    rc = entropyfs_blob_get(h, junk, &buf, &len);
    check(rc == EFS_NOT_FOUND && buf == NULL, "missing blob -> EFS_NOT_FOUND");
    check(strlen(last_err()) > 0, "last_error carries detail");

    /* lifecycle: close -> reopen -> close */
    rc = entropyfs_engine_close(h);
    check(rc == EFS_OK, "close");
    h = NULL;
    rc = entropyfs_engine_open("/tmp/efs-smoke-store", EFS_ENGINE_OPEN, &h);
    check(rc == EFS_OK && h != NULL, "reopen existing store");
    if (rc == EFS_OK && h) {
        rc = entropyfs_blob_get(h, id_a, &buf, &len);
        check(rc == EFS_OK && len == strlen(blob_a) && memcmp(buf, blob_a, len) == 0,
              "blob survives close/reopen byte-exact");
        entropyfs_free(buf);
        rc = entropyfs_engine_close(h);
        check(rc == EFS_OK, "close again");
    }

    printf(failures == 0 ? "== C ABI smoke PASS ==\n" : "== C ABI smoke FAIL (%d) ==\n",
           failures);
    return failures == 0 ? 0 : 1;
}
