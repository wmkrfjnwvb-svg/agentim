// The AAR in an app (docs/12 §2.12.1, sdk/client-kotlin/android/test-app.sh).
pluginManagement {
    repositories { google(); mavenCentral(); gradlePluginPortal() }
}
dependencyResolutionManagement {
    repositories { google(); mavenCentral() }
}
rootProject.name = "agentim-aar-test"
include(":app")
