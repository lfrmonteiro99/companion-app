package com.companion.awareness

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

class LogsActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    // `tick` forces a re-read when the user clears or the
                    // activity resumes. Reading the whole file on each tick
                    // is fine — it's capped at ~1 MB.
                    var tick by remember { mutableStateOf(0) }
                    val entries = remember(tick) { AppLog.recent(this@LogsActivity) }

                    LaunchedEffect(Unit) { tick++ }

                    Column(modifier = Modifier.fillMaxSize().padding(12.dp)) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Text(
                                "App log (${entries.size})",
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Box(modifier = Modifier.weight(1f))
                            OutlinedButton(onClick = { tick++ }) { Text("Refresh") }
                            Button(onClick = {
                                AppLog.clear(this@LogsActivity)
                                tick++
                            }) { Text("Clear") }
                        }
                        if (entries.isEmpty()) {
                            Box(
                                modifier = Modifier.fillMaxSize(),
                                contentAlignment = Alignment.Center,
                            ) {
                                Text(
                                    "No log entries. Errors, warnings and crashes show up here.",
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                            }
                        } else {
                            LazyColumn(
                                modifier = Modifier.fillMaxSize(),
                                verticalArrangement = Arrangement.spacedBy(10.dp),
                            ) {
                                items(entries) { entry ->
                                    LogRow(entry)
                                    HorizontalDivider()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LogRow(entry: AppLog.Entry) {
    val time = runCatching {
        Instant.parse(entry.timestamp)
            .atZone(ZoneId.systemDefault())
            .format(TIME_FMT)
    }.getOrNull() ?: entry.timestamp

    val accent = when (entry.level) {
        AppLog.Level.INFO -> Color(0xFF6E6E6E)
        AppLog.Level.WARN -> Color(0xFFB8860B)
        AppLog.Level.ERROR -> Color(0xFFCC3333)
        AppLog.Level.CRASH -> Color(0xFF8B0000)
    }

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                entry.level.name,
                style = MaterialTheme.typography.labelSmall.copy(
                    fontWeight = FontWeight.Bold,
                    color = Color.White,
                ),
                modifier = Modifier
                    .background(accent)
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            )
            Text(
                "$time · ${entry.tag}",
                style = MaterialTheme.typography.labelMedium,
            )
        }
        Text(
            entry.message,
            style = MaterialTheme.typography.bodyMedium,
        )
        entry.stackTrace?.let { trace ->
            Text(
                trace,
                style = MaterialTheme.typography.bodySmall.copy(
                    fontFamily = FontFamily.Monospace,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFFF5F5F5))
                    .padding(8.dp),
            )
        }
    }
}

private val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("dd MMM HH:mm:ss")
