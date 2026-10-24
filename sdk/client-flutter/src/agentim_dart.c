/* Dart FFI shim over the C ABI (sdk/client-rust/native/include/agentim.h).
 * Dart receives events through NativeCallable.listener, which runs after
 * the native call returned, when the core's string is gone: each event is
 * copied here and Dart frees the copy (agentim_dart_string_free). */

#include <stdlib.h>
#include <string.h>

#include "agentim.h"

typedef void (*agentim_dart_cb)(char *json);

typedef struct {
    agentim_client *client;
    agentim_dart_cb cb;
} agentim_dart;

static void relay(void *ctx, const char *json) {
    agentim_dart *h = ctx;
    size_t n = strlen(json) + 1;
    char *copy = malloc(n);
    if (!copy) return;
    memcpy(copy, json, n);
    h->cb(copy);
}

agentim_dart *agentim_dart_new(const char *config, const char *store_dir, agentim_dart_cb cb) {
    agentim_dart *h = calloc(1, sizeof *h);
    if (!h) return NULL;
    h->cb = cb;
    h->client = agentim_client_new(config, store_dir, relay, h);
    if (!h->client) {
        free(h);
        return NULL;
    }
    return h;
}

agentim_client *agentim_dart_client(agentim_dart *h) { return h->client; }

/* No event is relayed after this returns. */
void agentim_dart_free(agentim_dart *h) {
    agentim_client_free(h->client);
    free(h);
}

void agentim_dart_string_free(char *s) { free(s); }
