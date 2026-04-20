package com.companion.awareness

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.json.JSONObject

/**
 * Profile editor. Shows the free-text bio the user controls plus the
 * auto-learned interests / anti-interests / top-apps that the rating
 * actions and tick loop have been accumulating.
 */
class ProfileActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var bio by remember { mutableStateOf(Settings.userBio(this@ProfileActivity)) }
                    var summary by remember { mutableStateOf("") }

                    // Pull the current profile from the Rust core every
                    // time the activity comes to the front so the user
                    // sees rating additions land in real time.
                    LaunchedEffect(Unit) {
                        summary = runCatching { CoreBridge.getProfileText() }.getOrDefault("{}")
                    }

                    Column(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(20.dp)
                            .verticalScroll(rememberScrollState()),
                        verticalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        Text(
                            "Profile",
                            style = MaterialTheme.typography.titleLarge,
                        )
                        Text(
                            "O que escreveres aqui é injectado no system prompt do modelo em cada tick. " +
                                "Descreve quem és, interesses, trabalho, o que queres que a app observe.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        OutlinedTextField(
                            value = bio,
                            onValueChange = { bio = it },
                            label = { Text("Sobre ti") },
                            modifier = Modifier.fillMaxSize(),
                            minLines = 6,
                        )
                        Button(onClick = {
                            Settings.setUserBio(this@ProfileActivity, bio)
                            runCatching { CoreBridge.setBio(bio) }
                            summary = runCatching { CoreBridge.getProfileText() }.getOrDefault("{}")
                        }) {
                            Text("Guardar")
                        }

                        HorizontalDivider()
                        Text(
                            "Aprendido por rating (botões 'Mais disto' / 'Não interessa' nas notificações)",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        ProfileSummary(summary)
                    }
                }
            }
        }
    }
}

@androidx.compose.runtime.Composable
private fun ProfileSummary(json: String) {
    val obj = runCatching { JSONObject(json) }.getOrNull()
    if (obj == null) {
        Text("—", style = MaterialTheme.typography.bodySmall)
        return
    }
    val interests = obj.optJSONArray("interests")
    val antiInterests = obj.optJSONArray("anti_interests")
    val topApps = obj.optString("top_apps", "")

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (interests != null && interests.length() > 0) {
            Text("Interesses confirmados:", style = MaterialTheme.typography.labelMedium)
            for (i in 0 until interests.length()) {
                Text("• ${interests.optString(i)}", style = MaterialTheme.typography.bodySmall)
            }
        }
        if (antiInterests != null && antiInterests.length() > 0) {
            Text("Não interessa:", style = MaterialTheme.typography.labelMedium)
            for (i in 0 until antiInterests.length()) {
                Text("• ${antiInterests.optString(i)}", style = MaterialTheme.typography.bodySmall)
            }
        }
        if (topApps.isNotBlank()) {
            Text(
                topApps,
                style = MaterialTheme.typography.bodySmall,
            )
        }
        if ((interests == null || interests.length() == 0) &&
            (antiInterests == null || antiInterests.length() == 0) &&
            topApps.isBlank()
        ) {
            Text(
                "Sem dados ainda — usa a app alguns minutos e carrega nos botões das notificações para ensinar o que queres ouvir.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}
