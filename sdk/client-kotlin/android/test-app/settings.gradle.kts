// The AAR in an app (docs/en/design/developer-experience.md, sdk/client-kotlin/android/test-app.sh).
pluginManagement {
    repositories { google(); mavenCentral(); gradlePluginPortal() }
}
dependencyResolutionManagement {
    repositories { google(); mavenCentral() }
}
rootProject.name = "agentim-aar-test"
include(":app")
