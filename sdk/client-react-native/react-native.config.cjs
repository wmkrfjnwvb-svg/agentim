// Autolinking (docs/12 §2.12): Android takes the pure C++ module from
// android/CMakeLists.txt (no Gradle project); iOS takes the podspec.
module.exports = {
  dependency: {
    platforms: {
      android: {
        sourceDir: 'android',
        cxxModuleCMakeListsPath: 'CMakeLists.txt',
        cxxModuleCMakeListsModuleName: 'agentim_rn',
        cxxModuleHeaderName: 'NativeAgentIM',
      },
    },
  },
};
