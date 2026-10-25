// The TurboModule React Native links (docs/12 §2.12): a pure C++ module whose
// install() puts the JSI binding (AgentIMJsi.h) on global.__agentim. Android
// autolinking includes this header and makes the class by its name.
#pragma once

#include <AgentIMSpecJSI.h>

#include <memory>

namespace facebook::react {

class NativeAgentIM : public NativeAgentIMCxxSpec<NativeAgentIM> {
 public:
  explicit NativeAgentIM(std::shared_ptr<CallInvoker> jsInvoker);
  bool install(jsi::Runtime &rt);
};

} // namespace facebook::react
