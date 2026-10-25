# iOS (docs/12 §2.12): the C++ TurboModule and the JSI binding, with the
# native core as a dynamic XCFramework (ios/AgentIMCore.xcframework, put in
# the npm package by prepare-native.sh).
require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "AgentIMReactNative"
  s.version      = package["version"]
  s.summary      = package["description"]
  s.license      = package["license"]
  s.homepage     = "https://github.com/wmkrfjnwvb-svg/agentim"
  s.authors      = "AgentIM"
  s.platforms    = { :ios => "17.0" }
  s.source       = { :git => "https://github.com/wmkrfjnwvb-svg/agentim.git", :tag => "sdk-v#{s.version}" }
  s.source_files = "cpp/**/*.{h,cpp}", "ios/**/*.{h,mm}", "include/agentim.h"
  s.vendored_frameworks = "ios/AgentIMCore.xcframework"
  s.pod_target_xcconfig = { "CLANG_CXX_LANGUAGE_STANDARD" => "c++20" }
  install_modules_dependencies(s)
end
