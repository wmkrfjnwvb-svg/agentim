/* The C ABI from C: connect with a token, create a conversation, send,
 * and see the message sent. Prints what happened; exits 0 on success.
 *   abi_test <url> <token> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
#include "agentim.h"

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static agentim_client *client;
static const char *token;
static int connected, sent, created;
static char conv[128];

static void on_event(void *ctx, const char *json) {
    (void)ctx;
    pthread_mutex_lock(&mu);
    if (strstr(json, "\"type\":\"need_token\"")) agentim_client_token(client, token, 0);
    if (strstr(json, "\"type\":\"connection\"") && strstr(json, "\"state\":\"connected\"")) connected = 1;
    const char *c = strstr(json, "\"conv_id\":\"");
    if (strstr(json, "\"id\":2") && strstr(json, "\"type\":\"reply\"") && c && !created) {
        c += 11;
        size_t n = strcspn(c, "\"");
        if (n < sizeof conv) { memcpy(conv, c, n); conv[n] = 0; created = 1; }
    }
    if (strstr(json, "\"type\":\"operation\"") && strstr(json, "\"result\":\"done\"")) sent = 1;
    pthread_mutex_unlock(&mu);
}

static int wait_for(int *flag) {
    for (int i = 0; i < 200; i++) {
        pthread_mutex_lock(&mu);
        int v = *flag;
        pthread_mutex_unlock(&mu);
        if (v) return 1;
        usleep(50000);
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    token = argv[2];
    char cfg[512];
    snprintf(cfg, sizeof cfg, "{\"url\":\"%s\",\"device_id\":\"c-device\"}", argv[1]);
    printf("version %s\n", agentim_version());
    if (agentim_client_new("not json", NULL, on_event, NULL) != NULL) return 3;
    pthread_mutex_lock(&mu);
    client = agentim_client_new(cfg, NULL, on_event, NULL);
    pthread_mutex_unlock(&mu);
    if (!client) return 4;
    if (agentim_client_command(client, 99, "nope") != -1) return 5;
    agentim_client_command(client, 1, "{\"cmd\":\"connect\"}");
    if (!wait_for(&connected)) { printf("not connected\n"); return 6; }
    printf("connected\n");
    agentim_client_command(client, 2, "{\"cmd\":\"call\",\"op\":\"create_conv\",\"args\":{\"kind\":\"group\",\"title\":\"c\",\"members\":[]}}");
    if (!wait_for(&created)) { printf("no conversation\n"); return 7; }
    char cmd[512];
    snprintf(cmd, sizeof cmd, "{\"cmd\":\"send\",\"conv_id\":\"%s\",\"text\":\"from C\"}", conv);
    agentim_client_command(client, 3, cmd);
    if (!wait_for(&sent)) { printf("not sent\n"); return 8; }
    printf("sent\n");
    agentim_client_network(client, 1, 1);
    agentim_client_foreground(client, 1);
    agentim_client_free(client);
    agentim_client_free(NULL);
    return 0;
}
