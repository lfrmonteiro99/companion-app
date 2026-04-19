plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Optional build-time OpenAI key. When the OPENAI_API_KEY env var is
// set (CI populates it from a GitHub secret; a dev can `export` it
// before running ./gradlew) we expose it via BuildConfig so the app
// can auto-configure the core on first launch. Empty string → user
// must enter the key in the UI, same as a plain install.
//
// SECURITY: the key ends up as a plain string inside the APK's
// BuildConfig.class. Anyone with the APK can extract it with apktool
// in under a minute. Only use this when the APK stays on your own
// device (personal build, private distribution).
val bakedOpenAiKey: String = System.getenv("OPENAI_API_KEY") ?: ""

android {
    namespace = "com.companion.awareness"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.companion.awareness"
        minSdk = 29
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }

        buildConfigField("String", "OPENAI_API_KEY", "\"$bakedOpenAiKey\"")
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
        buildConfig = true
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
