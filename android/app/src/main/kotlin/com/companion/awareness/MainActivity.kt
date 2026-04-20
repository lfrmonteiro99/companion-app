package com.companion.awareness

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {

    private val projectionLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == Activity.RESULT_OK && result.data != null) {
            AwarenessService.start(this, result.resultCode, result.data!!)
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
    ) { _ ->
        // Regardless of which permissions the user granted, proceed to the
        // MediaProjection prompt. The service adapts its fgServiceType and
        // AudioCapture is a no-op when RECORD_AUDIO is denied.
        val mpm = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        projectionLauncher.launch(mpm.createScreenCaptureIntent())
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        CoreBridge.init()

        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var status by remember { mutableStateOf("idle") }
                    var apiKey by remember { mutableStateOf(Settings.openAiKey(this@MainActivity)) }
                    var usageGranted by remember { mutableStateOf(FocusedApp.isGranted(this@MainActivity)) }
                    var a11yEnabled by remember { mutableStateOf(AwarenessAccessibilityService.isConnected()) }
                    var ttsEnabled by remember { mutableStateOf(Settings.ttsEnabled(this@MainActivity)) }

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
                            "Accessibility: " + if (a11yEnabled) "on (cleaner text)" else "off (using OCR)",
                        )
                        if (!a11yEnabled) {
                            Button(onClick = {
                                startActivity(Intent(SystemSettings.ACTION_ACCESSIBILITY_SETTINGS))
                            }) {
                                Text("Enable accessibility")
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
                        Text("Status: $status")
                        Button(
                            enabled = apiKey.isNotBlank(),
                            onClick = {
                                usageGranted = FocusedApp.isGranted(this@MainActivity)
                                a11yEnabled = AwarenessAccessibilityService.isConnected()
                                status = "requesting permissions…"
                                if (hasAllRuntimePermissions()) {
                                    val mpm = getSystemService(MEDIA_PROJECTION_SERVICE)
                                        as MediaProjectionManager
                                    projectionLauncher.launch(mpm.createScreenCaptureIntent())
                                } else {
                                    // Request RECORD_AUDIO + POST_NOTIFICATIONS
                                    // first; the callback then launches the
                                    // MediaProjection picker. Doing both in
                                    // the same onClick would race and the
                                    // foreground service crash on Android 14+.
                                    runtimePermissionsLauncher.launch(requiredRuntimePermissions())
                                }
                            },
                        ) {
                            Text("Start capture")
                        }
                        Button(onClick = {
                            AwarenessService.stop(this@MainActivity)
                            status = "stopped"
                        }) {
                            Text("Stop")
                        }
                        Button(onClick = {
                            startActivity(Intent(this@MainActivity, HistoryActivity::class.java))
                        }) {
                            Text("View alert history")
                        }
                    }
                }
            }
        }
    }
}
