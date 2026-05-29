package com.companion.awareness

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings as SystemSettings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.foundation.layout.Row
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {

    // Shared with the Compose Status label so launcher callbacks can
    // reflect reality — without this, clicking Cancel in the
    // MediaProjection dialog left the UI stuck on "requesting permissions…"
    // forever because the label only updated from the button onClick.
    private val status: MutableState<String> = mutableStateOf("idle")

    private val projectionLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == Activity.RESULT_OK && result.data != null) {
            AppLog.i(TAG, "MediaProjection granted; starting service")
            AwarenessService.start(this, result.resultCode, result.data!!)
            status.value = "capturing"
        } else {
            AppLog.w(TAG, "MediaProjection denied or cancelled (resultCode=${result.resultCode})")
            status.value = "capture cancelled — tap Start to try again"
        }
    }

    // Runtime permissions requested in one batch BEFORE the MediaProjection
    // dialog. Sequence matters on Android 14+: if we start the foreground
    // service with foregroundServiceType=microphone while RECORD_AUDIO is
    // still unapproved, the OS throws SecurityException and the user sees
    // "Awareness keeps stopping". POST_NOTIFICATIONS is a no-op on <13 and
    // required on 13+ (otherwise the foreground service notification and
    // every alert are silently dropped).
    private val runtimePermissionsLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { grants ->
        val granted = grants.entries.joinToString(",") { "${it.key.substringAfterLast('.')}=${it.value}" }
        AppLog.i(TAG, "runtime permissions result: $granted")
        dispatchPostPermissions()
    }

    private fun requiredRuntimePermissions(): Array<String> {
        val perms = mutableListOf(android.Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            perms += android.Manifest.permission.POST_NOTIFICATIONS
        }
        return perms.toTypedArray()
    }

    private fun hasAllRuntimePermissions(): Boolean =
        requiredRuntimePermissions().all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }

    private fun isBatteryOptimized(): Boolean {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        return !pm.isIgnoringBatteryOptimizations(packageName)
    }

    /**
     * Open the direct "allow background activity" flow for this app. Samsung
     * One UI (and stock Android) normally routes us through a one-tap
     * system dialog when we use ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS
     * with a package URI. Fall back to the generic list screen on any
     * device that rejects the direct action.
     */
    private fun openBatteryOptimizationSettings() {
        val direct = Intent(
            SystemSettings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
        ).apply {
            data = Uri.parse("package:$packageName")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        val fallback = Intent(SystemSettings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { startActivity(direct) }
            .onFailure { startActivity(fallback) }
    }

    companion object {
        private const val TAG = "MainActivity"
    }

    private fun startCaptureFlow() {
        // Always gate on runtime permissions first — even in a11y-only
        // mode we still want RECORD_AUDIO (so SpeechRecognizer can
        // feed mic_text_recent into the gate's voice_activity /
        // emotional rules) and POST_NOTIFICATIONS (so alerts actually
        // show). The callback picks the right start method afterwards.
        if (!hasAllRuntimePermissions()) {
            AppLog.i(TAG, "missing runtime permissions → prompting")
            status.value = "requesting permissions…"
            runtimePermissionsLauncher.launch(requiredRuntimePermissions())
            return
        }
        dispatchPostPermissions()
    }

    private fun dispatchPostPermissions() {
        // Preferred path: accessibility service already enabled → start
        // the service directly, no MediaProjection consent dialog, no
        // shutter sound, no keyguard-kill risk. Fallback: legacy
        // MediaProjection flow for users who didn't enable a11y.
        if (AwarenessAccessibilityService.isConnected()) {
            AppLog.i(TAG, "a11y connected + permissions ok → a11y-only start")
            AwarenessService.startWithoutProjection(this)
            status.value = "capturing (accessibility)"
        } else {
            AppLog.i(TAG, "a11y not connected → requesting MediaProjection")
            val mpm = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
            status.value = "requesting screen access…"
            projectionLauncher.launch(mpm.createScreenCaptureIntent())
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        CoreBridge.init()

        // Tap-to-resume from the "Captura parou" notification. The
        // service-side notification put EXTRA_AUTO_START=true on the
        // PendingIntent; if a stored key is already present we trigger
        // the same flow the Start button does, no extra clicks.
        if (intent?.getBooleanExtra(AwarenessService.EXTRA_AUTO_START, false) == true &&
            Settings.openAiKey(this).isNotBlank()
        ) {
            startCaptureFlow()
        }

        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    val statusText by status
                    var apiKey by remember { mutableStateOf(Settings.openAiKey(this@MainActivity)) }
                    var usageGranted by remember { mutableStateOf(FocusedApp.isGranted(this@MainActivity)) }
                    var a11yEnabled by remember { mutableStateOf(AwarenessAccessibilityService.isConnected()) }
                    var ttsEnabled by remember { mutableStateOf(Settings.ttsEnabled(this@MainActivity)) }
                    var budgetText by remember {
                        mutableStateOf("%.2f".format(Settings.budgetUsdDaily(this@MainActivity)))
                    }

                    Column(
                        modifier = Modifier.fillMaxSize().padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text("Awareness (Android)", style = MaterialTheme.typography.titleLarge)
                        OutlinedTextField(
                            value = apiKey,
                            onValueChange = {
                                apiKey = it
                                Settings.setOpenAiKey(this@MainActivity, it)
                            },
                            label = { Text("OpenAI API key") },
                            visualTransformation = PasswordVisualTransformation(),
                            singleLine = true,
                        )
                        Text(
                            "Usage access: " + if (usageGranted) "granted" else "not granted (app name will be blank)",
                        )
                        if (!usageGranted) {
                            Button(onClick = {
                                startActivity(Intent(SystemSettings.ACTION_USAGE_ACCESS_SETTINGS))
                            }) {
                                Text("Grant usage access")
                            }
                        }
                        Text(
                            if (a11yEnabled) {
                                "Accessibility: on — capture survives background and lock screen"
                            } else {
                                "Accessibility: OFF — REQUIRED so the OS doesn't kill capture. Tap below to enable."
                            },
                        )
                        if (!a11yEnabled) {
                            Button(onClick = {
                                startActivity(Intent(SystemSettings.ACTION_ACCESSIBILITY_SETTINGS))
                            }) {
                                Text("Enable accessibility (required)")
                            }
                        }
                        var batteryOptimized by remember {
                            mutableStateOf(isBatteryOptimized())
                        }
                        Text(
                            if (batteryOptimized) {
                                "Battery: app is being optimized — capture will be paused when backgrounded (Samsung). Disable for stable capture."
                            } else {
                                "Battery: unrestricted — capture can run in background."
                            },
                        )
                        if (batteryOptimized) {
                            Button(onClick = {
                                openBatteryOptimizationSettings()
                                // The result isn't returned as an Activity
                                // result; re-check on resume by polling.
                                batteryOptimized = isBatteryOptimized()
                            }) {
                                Text("Allow background activity")
                            }
                        }
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Text("Speak alerts")
                            Switch(
                                checked = ttsEnabled,
                                onCheckedChange = {
                                    ttsEnabled = it
                                    Settings.setTtsEnabled(this@MainActivity, it)
                                },
                            )
                        }
                        OutlinedTextField(
                            value = budgetText,
                            onValueChange = { raw ->
                                // Accept comma as decimal separator (pt-PT
                                // keyboards default to that). Strip anything
                                // that isn't a digit or a decimal point so
                                // the field can't hold non-numeric junk.
                                val cleaned = raw.replace(',', '.')
                                    .filter { it.isDigit() || it == '.' }
                                budgetText = cleaned
                                cleaned.toDoubleOrNull()?.let {
                                    Settings.setBudgetUsdDaily(this@MainActivity, it)
                                }
                            },
                            label = { Text("Daily budget USD (local cap, not OpenAI)") },
                            singleLine = true,
                        )
                        Text(
                            "Applies on next Start — Stop and Start capture after changing.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Text("Status: $statusText")
                        Button(
                            enabled = apiKey.isNotBlank(),
                            onClick = {
                                usageGranted = FocusedApp.isGranted(this@MainActivity)
                                a11yEnabled = AwarenessAccessibilityService.isConnected()
                                startCaptureFlow()
                            },
                        ) {
                            Text("Start capture")
                        }
                        Button(onClick = {
                            AwarenessService.stop(this@MainActivity)
                            status.value = "stopped"
                        }) {
                            Text("Stop")
                        }
                        Button(onClick = {
                            startActivity(Intent(this@MainActivity, HistoryActivity::class.java))
                        }) {
                            Text("View alert history")
                        }
                        Button(onClick = {
                            startActivity(Intent(this@MainActivity, LogsActivity::class.java))
                        }) {
                            Text("View app log")
                        }
                        Button(onClick = {
                            startActivity(Intent(this@MainActivity, TraceActivity::class.java))
                        }) {
                            Text("View pipeline trace")
                        }
                        Button(onClick = {
                            startActivity(Intent(this@MainActivity, ProfileActivity::class.java))
                        }) {
                            Text("Edit profile")
                        }
                    }
                }
            }
        }
    }
}
