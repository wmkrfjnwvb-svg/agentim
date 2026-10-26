module.exports = {
  preset: '@react-native/jest-preset',
  testMatch: ['<rootDir>/test/**/*.test.tsx'],
  // The SDK packages are ES modules: compile them like the app's code.
  transformIgnorePatterns: ['node_modules/(?!((jest-)?react-native|@react-native(-community)?|@agentimhq)/)'],
  // They are linked from sdk/: resolve what they import (Babel's helpers,
  // React) here, so there is one React, as in an app.
  modulePaths: ['<rootDir>/node_modules'],
  moduleNameMapper: { '^react$': '<rootDir>/node_modules/react', '^react/(.*)$': '<rootDir>/node_modules/react/$1' },
};
