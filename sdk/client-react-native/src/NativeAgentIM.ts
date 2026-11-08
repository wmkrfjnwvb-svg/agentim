// The TurboModule (React Native codegen reads this file): a pure C++ module
// (cpp/NativeAgentIM.cpp) whose install() puts the JSI binding on
// global.__agentim.
import type { TurboModule } from 'react-native';
import { TurboModuleRegistry } from 'react-native';

export interface Spec extends TurboModule {
  readonly install: () => boolean;
}

export default TurboModuleRegistry.getEnforcing<Spec>('AgentIM');
