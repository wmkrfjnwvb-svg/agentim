// JSI binding for React Native (docs/12 §2.12): installs `global.__agentim`,
// a thin layer over the C ABI (sdk/client-rust/native/include/agentim.h).
// Events come from the SDK's thread and are handed to the JS thread through
// the CallInvoker, in order.
#pragma once

#include <memory>

#include <ReactCommon/CallInvoker.h>
#include <jsi/jsi.h>

namespace agentim {

void install(facebook::jsi::Runtime &rt, std::shared_ptr<facebook::react::CallInvoker> invoker);

} // namespace agentim
