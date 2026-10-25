/* AgentIM client SDK, C ABI (docs/12 §2.2). JSON in, JSON out.
 *
 *   agentim_client *c = agentim_client_new(
 *       "{\"url\":\"https://im.example.com\",\"device_id\":\"phone-1\"}",
 *       "/path/to/cache", on_event, ctx);
 *   agentim_client_command(c, 1, "{\"cmd\":\"connect\"}");
 *
 * Events (`{"type": …}`) arrive on the SDK's own thread: `need_token`
 * (answer with agentim_client_token), `connection`, `messages`,
 * `conversations`, `operation`, `side`, `signal`, `reset`, `storage`,
 * `event`, and `reply` (the answer to a command, by id).
 */
#ifndef AGENTIM_H
#define AGENTIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct agentim_client agentim_client;
typedef void (*agentim_event_cb)(void *ctx, const char *json);

agentim_client *agentim_client_new(const char *config_json, const char *store_dir,
                                   agentim_event_cb callback, void *ctx);
int agentim_client_command(agentim_client *c, uint64_t id, const char *cmd_json);
void agentim_client_token(agentim_client *c, const char *token, int refused);
void agentim_client_network(agentim_client *c, int online, uint64_t generation);
void agentim_client_foreground(agentim_client *c, int foreground);
void agentim_client_free(agentim_client *c);
const char *agentim_version(void);

/* Dart: each event is a copy the callback owns; free it with
 * agentim_dart_string_free (Dart reads it after the call returned). */
typedef void (*agentim_dart_event_cb)(char *json);
agentim_client *agentim_dart_new(const char *config_json, const char *store_dir,
                                 agentim_dart_event_cb callback);
void agentim_dart_string_free(char *json);

#ifdef __cplusplus
}
#endif

#endif
