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

    // Release signing. CI supplies four properties via
    // `ORG_GRADLE_PROJECT_awareness<*>` env vars sourced from GitHub
    // secrets. When any one is missing we skip wiring the signingConfig,
    // so local `assembleRelease` runs still fall back to the debug
    // keystore (zero developer setup) instead of failing to build.
    val releaseKeystorePath =
        providers.gradleProperty("awarenessReleaseKeystore").orNull
    val releaseKeystorePassword =
        providers.gradleProperty("awarenessReleaseKeystorePassword").orNull
    val releaseKeyAlias =
        providers.gradleProperty("awarenessReleaseKeyAlias").orNull
    val releaseKeyPassword =
        providers.gradleProperty("awarenessReleaseKeyPassword").orNull
    val hasReleaseSigning = listOf(
        releaseKeystorePath,
        releaseKeystorePassword,
        releaseKeyAlias,
        releaseKeyPassword,
    ).all { !it.isNullOrBlank() }

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

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(releaseKeystorePath!!)
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            }
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

    // WorkManager — resilient to process kills (the OS restarts the worker
    // after Samsung / Doze terminates our foreground service). Used as the
    // dead-man's-switch for the capture pipeline.
    implementation("androidx.work:work-runtime-ktx:2.9.1")
}
