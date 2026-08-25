#define ENTROPYFS_WORKLOAD_FOO 1
#define ENTROPYFS_WORKLOAD_BAR 2

typedef struct entropy_workload_s {
    unsigned long long magic;
    unsigned int flags;
    unsigned char data[64];
} entropy_workload_t;

void entropy_workload_init(entropy_workload_t *w);
int entropy_workload_run(const entropy_workload_t *w, unsigned long long ops);
