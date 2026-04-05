// QuickLSP evaluation fixture: kernel-style C code.
// Exercises #ifdef-guarded definitions, macros, local variables, and
// symbols defined only in external headers (like printk).

#include <linux/kernel.h>
#include <linux/module.h>

#define CIA_VERSION 1
#define CIA_MAX_OPS 16
#define CIA_MIN(a, b) ((a) < (b) ? (a) : (b))

enum cia_op_type {
    CIA_READ,
    CIA_WRITE,
    CIA_EXEC
};

struct cia_context {
    int op_count;
    enum cia_op_type last_op;
    char name[64];
};

typedef struct cia_context cia_ctx_t;

#ifdef CONFIG_CIA_SECURITY
struct cia_security_ops {
    int (*check)(struct cia_context *ctx);
    void (*audit)(struct cia_context *ctx, int result);
};

static int cia_security_check(struct cia_context *ctx) {
    int result = 0;
    if (ctx->op_count > CIA_MAX_OPS) {
        result = -1;
    }
    return result;
}

void cia_security_audit(struct cia_context *ctx, int result) {
    // Would call printk in real kernel code:
    // printk(KERN_INFO "CIA audit: op_count=%d result=%d\n", ctx->op_count, result);
}
#endif

#ifndef CONFIG_CIA_MINIMAL
int cia_full_init(struct cia_context *ctx) {
    ctx->op_count = 0;
    ctx->last_op = CIA_READ;
    return 0;
}
#endif

#if defined(CONFIG_SMP)
typedef struct {
    int lock;
} cia_spinlock_t;

static int cia_spin_lock(cia_spinlock_t *lock) {
    lock->lock = 1;
    return 0;
}

#ifdef CONFIG_DEBUG_SPINLOCK
void cia_spin_dump(cia_spinlock_t *lock) {
    // printk(KERN_DEBUG "lock=%d\n", lock->lock);
}
#endif

#else
void cia_nosmp_fallback(void) {}
#endif

/// Initialize a CIA context.
void cia_init(struct cia_context *ctx, const char *name) {
    ctx->op_count = 0;
    ctx->last_op = CIA_READ;
    // strncpy would be used here
}

/// Execute a CIA operation.
int cia_execute(struct cia_context *ctx, enum cia_op_type op) {
    int status = 0;
    int prev_count = ctx->op_count;
    ctx->last_op = op;
    ctx->op_count++;
    if (ctx->op_count > CIA_MAX_OPS) {
        status = -1;
    }
    return status;
}

/// Main entry point for CIA module.
int cia_main(void) {
    struct cia_context ctx;
    cia_init(&ctx, "main");
    int result = cia_execute(&ctx, CIA_WRITE);
    if (result != 0) {
        // printk(KERN_ERR "cia_execute failed\n");
        return result;
    }
    return 0;
}
