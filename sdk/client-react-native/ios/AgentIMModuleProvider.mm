#import "AgentIMModuleProvider.h"

#import <ReactCommon/CallInvoker.h>
#import <ReactCommon/TurboModule.h>

#import "NativeAgentIM.h"

@implementation AgentIMModuleProvider

- (std::shared_ptr<facebook::react::TurboModule>)getTurboModule:
    (const facebook::react::ObjCTurboModule::InitParams &)params
{
  return std::make_shared<facebook::react::NativeAgentIM>(params.jsInvoker);
}

@end
