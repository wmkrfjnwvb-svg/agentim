#include "NativeAgentIM.h"

#include "AgentIMJsi.h"

namespace facebook::react {

NativeAgentIM::NativeAgentIM(std::shared_ptr<CallInvoker> jsInvoker)
    : NativeAgentIMCxxSpec(std::move(jsInvoker)) {}

bool NativeAgentIM::install(jsi::Runtime &rt) {
  agentim::install(rt, jsInvoker_);
  return true;
}

} // namespace facebook::react
