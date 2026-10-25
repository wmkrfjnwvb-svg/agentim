plugins {
    id("com.android.application") version "9.4.1"
}

android {
    namespace = "com.agentim.aartest"
    compileSdk = 35
    defaultConfig {
        applicationId = "com.agentim.aartest"
        minSdk = 24
        targetSdk = 35
        val url = providers.gradleProperty("agentimUrl").getOrElse("http://127.0.0.1:8080")
        buildConfigField("String", "AGENTIM_URL", "\"$url\"")
    }
    buildFeatures { buildConfig = true }
}

dependencies {
    // The library as an app gets it: the AAR file (Maven Central later, D126).
    implementation(files("libs/agentim-client.aar"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
}
