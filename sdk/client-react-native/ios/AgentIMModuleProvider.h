#import <Foundation/Foundation.h>
#import <ReactCommon/RCTTurboModule.h>

NS_ASSUME_NONNULL_BEGIN

// Gives React Native the C++ module (package.json codegenConfig.ios.modulesProvider).
@interface AgentIMModuleProvider : NSObject <RCTModuleProvider>
@end

NS_ASSUME_NONNULL_END
