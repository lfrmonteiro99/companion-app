plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.companion.awareness"
    compileSdk = 34

    // CI passes `-PawarenessVersionCode=<run_number>` and
    // `-PawarenessVersionName=0.1.<run_number>` so the APK's internal
    // version matches the GitHub release tag. Local `./gradlew` builds
    // (no properties supplied) keep the 1 / 0.1.0 fallback so nothing
    // regresses for developers building off-CI.
    val ciVersionCode = providers.gradleProperty("awarenessVersionCode").orNull?.toIntOrNull()
    val ciVersionName = providers.gradleProperty("awarenessVersionName").orNull

    defaultConfig {
        applicationId = "com.companion.awareness"
        minSdk = 29
        targetSdk = 34
        versionCode = ciVersionCode ?: 1
        versionName = ciVersionName ?: "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
    }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2024.09.02"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.6")
    implementation("androidx.lifecycle:lifecycle-service:2.8.6")

    // On-device OCR for captured screens
    implementation("com.google.mlkit:text-recognition:16.0.1")

    // EncryptedSharedPreferences for the OpenAI key at rest
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    // Kotlin coroutines
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
