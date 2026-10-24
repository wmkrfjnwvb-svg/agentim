// global.__agentim:
//   create(configJson, storeDir | null, onEvent(json)) -> handle (number, 0 on bad config)
//   command(handle, id, cmdJson) -> 0 | -1
//   token(handle, token | null, refused)
//   network(handle, online, generation)
//   foreground(handle, foreground)
//   free(handle)              no event is delivered after it
//   version() -> string

#include "AgentIMJsi.h"

#include <atomic>
#include <mutex>
#include <string>
#include <unordered_map>
#include <utility>

#include "agentim.h"

using namespace facebook;

namespace agentim {
namespace {

struct Client {
  agentim_client *native = nullptr;
  std::shared_ptr<jsi::Function> onEvent;
  std::shared_ptr<react::CallInvoker> invoker;
  // Cleared by free(): events still queued for the JS thread are dropped.
  std::shared_ptr<std::atomic<bool>> alive = std::make_shared<std::atomic<bool>>(true);
};

std::mutex gate;
std::unordered_map<double, std::shared_ptr<Client>> clients;
double nextHandle = 0;

std::shared_ptr<Client> find(double h) {
  std::lock_guard<std::mutex> l(gate);
  auto it = clients.find(h);
  return it == clients.end() ? nullptr : it->second;
}

// On the SDK's thread: copy the event (its string dies with this call) and
// queue it for the JS thread.
void relay(void *ctx, const char *json) {
  auto *c = static_cast<Client *>(ctx);
  auto fn = c->onEvent;
  auto alive = c->alive;
  c->invoker->invokeAsync([fn, alive, text = std::string(json)](jsi::Runtime &rt) {
    if (!alive->load()) return;
    try {
      fn->call(rt, jsi::String::createFromUtf8(rt, text));
    } catch (const jsi::JSError &) {
      // The listener's own error; the JS side reports it.
    }
  });
}

std::string str(jsi::Runtime &rt, const jsi::Value &v) { return v.asString(rt).utf8(rt); }

jsi::Function fn(jsi::Runtime &rt, const char *name, unsigned args,
                 std::function<jsi::Value(jsi::Runtime &, const jsi::Value *, size_t)> body) {
  return jsi::Function::createFromHostFunction(
      rt, jsi::PropNameID::forAscii(rt, name), args,
      [body = std::move(body)](jsi::Runtime &rt, const jsi::Value &, const jsi::Value *a, size_t n) { return body(rt, a, n); });
}

} // namespace

void install(jsi::Runtime &rt, std::shared_ptr<react::CallInvoker> invoker) {
  jsi::Object api(rt);

  api.setProperty(rt, "create", fn(rt, "create", 3, [invoker](jsi::Runtime &rt, const jsi::Value *a, size_t n) -> jsi::Value {
    if (n < 3) throw jsi::JSError(rt, "create(config, storeDir, onEvent)");
    auto c = std::make_shared<Client>();
    c->onEvent = std::make_shared<jsi::Function>(a[2].asObject(rt).asFunction(rt));
    c->invoker = invoker;
    std::string cfg = str(rt, a[0]);
    std::string dir = a[1].isString() ? str(rt, a[1]) : std::string();
    c->native = agentim_client_new(cfg.c_str(), a[1].isString() ? dir.c_str() : nullptr, relay, c.get());
    if (!c->native) return jsi::Value(0);
    std::lock_guard<std::mutex> l(gate);
    double h = ++nextHandle;
    clients[h] = c;
    return jsi::Value(h);
  }));

  api.setProperty(rt, "command", fn(rt, "command", 3, [](jsi::Runtime &rt, const jsi::Value *a, size_t n) -> jsi::Value {
    auto c = n == 3 ? find(a[0].asNumber()) : nullptr;
    if (!c) return jsi::Value(-1);
    std::string cmd = str(rt, a[2]);
    return jsi::Value(agentim_client_command(c->native, static_cast<uint64_t>(a[1].asNumber()), cmd.c_str()));
  }));

  api.setProperty(rt, "token", fn(rt, "token", 3, [](jsi::Runtime &rt, const jsi::Value *a, size_t n) -> jsi::Value {
    auto c = n == 3 ? find(a[0].asNumber()) : nullptr;
    if (!c) return jsi::Value::undefined();
    if (a[1].isString()) {
      std::string t = str(rt, a[1]);
      agentim_client_token(c->native, t.c_str(), 0);
    } else {
      agentim_client_token(c->native, nullptr, a[2].getBool() ? 1 : 0);
    }
    return jsi::Value::undefined();
  }));

  api.setProperty(rt, "network", fn(rt, "network", 3, [](jsi::Runtime &, const jsi::Value *a, size_t n) -> jsi::Value {
    auto c = n == 3 ? find(a[0].asNumber()) : nullptr;
    if (c) agentim_client_network(c->native, a[1].getBool() ? 1 : 0, static_cast<uint64_t>(a[2].asNumber()));
    return jsi::Value::undefined();
  }));

  api.setProperty(rt, "foreground", fn(rt, "foreground", 2, [](jsi::Runtime &, const jsi::Value *a, size_t n) -> jsi::Value {
    auto c = n == 2 ? find(a[0].asNumber()) : nullptr;
    if (c) agentim_client_foreground(c->native, a[1].getBool() ? 1 : 0);
    return jsi::Value::undefined();
  }));

  api.setProperty(rt, "free", fn(rt, "free", 1, [](jsi::Runtime &, const jsi::Value *a, size_t n) -> jsi::Value {
    std::shared_ptr<Client> c;
    {
      std::lock_guard<std::mutex> l(gate);
      auto it = n == 1 ? clients.find(a[0].asNumber()) : clients.end();
      if (it == clients.end()) return jsi::Value::undefined();
      c = it->second;
      clients.erase(it);
    }
    c->alive->store(false);
    // Waits for the SDK's thread; no relay runs after it returns.
    agentim_client_free(c->native);
    return jsi::Value::undefined();
  }));

  api.setProperty(rt, "version", fn(rt, "version", 0, [](jsi::Runtime &rt, const jsi::Value *, size_t) -> jsi::Value {
    return jsi::String::createFromAscii(rt, agentim_version());
  }));

  rt.global().setProperty(rt, "__agentim", std::move(api));
}

} // namespace agentim
