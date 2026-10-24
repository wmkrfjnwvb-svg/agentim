# The Rust core ships as AgentIM.xcframework (built on macOS, docs/12 §2.12,
# A9); the shim is compiled from Classes/.
Pod::Spec.new do |s|
  s.name             = 'agentim_flutter'
  s.version          = '0.1.0'
  s.summary          = 'AgentIM client SDK for Flutter'
  s.homepage         = 'https://github.com/wmkrfjnwvb-svg/agentim'
  s.license          = { :type => 'Apache-2.0' }
  s.author           = { 'AgentIM' => 'dev@agentim.invalid' }
  s.source           = { :path => '.' }
  s.source_files     = 'Classes/**/*'
  s.vendored_frameworks = 'AgentIM.xcframework'
  s.dependency 'Flutter'
  s.platform = :ios, '13.0'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
  s.swift_version = '5.0'
end
