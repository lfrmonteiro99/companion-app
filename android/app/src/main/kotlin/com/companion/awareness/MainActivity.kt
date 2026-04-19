package com.companion.awareness

import android.app.Activity
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.os.Bundle
import android.provider.Settings as SystemSettings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
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

    private val micPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { /* user decides; service will no-op audio if denied */ }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        CoreBridge.init()

        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var status by remember { mutableStateOf("idle") }
                    var apiKey by remember { mutableStateOf(Settings.openAiKey(this@MainActivity)) }
                    var usageGranted by remember { mutableStateOf(FocusedApp.isGranted(this@MainActivity)) }

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
                        Text("Status: $status")
                        Button(
                            enabled = apiKey.isNotBlank(),
                            onClick = {
                                usageGranted = FocusedApp.isGranted(this@MainActivity)
                                micPermissionLauncher.launch(android.Manifest.permission.RECORD_AUDIO)
                                val mpm = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                                projectionLauncher.launch(mpm.createScreenCaptureIntent())
                                status = "requesting capture…"
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
                    }
                }
            }
        }
    }
}
