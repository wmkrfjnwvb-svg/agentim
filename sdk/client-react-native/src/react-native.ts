// The app entry (Metro reads the "react-native" field): installs the JSI
// binding once, then the same API as the other entry.
import NativeAgentIM from './NativeAgentIM';

if (!(globalThis as { __agentim?: unknown }).__agentim) NativeAgentIM.install();

export * from './client';
