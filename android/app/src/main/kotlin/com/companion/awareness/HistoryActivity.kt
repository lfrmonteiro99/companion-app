package com.companion.awareness

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

class HistoryActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var entries by remember { mutableStateOf(AlertLog.recent(this@HistoryActivity)) }
                    // Refresh on resume so the user sees new alerts
                    // without reopening the screen.
                    androidx.compose.runtime.LaunchedEffect(Unit) {
                        entries = AlertLog.recent(this@HistoryActivity)
                    }

                    if (entries.isEmpty()) {
                        Column(
                            modifier = Modifier.fillMaxSize().padding(24.dp),
                            verticalArrangement = Arrangement.Center,
                        ) {
                            Text(
                                "No alerts yet. Start capture and leave the app running.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    } else {
                        LazyColumn(
                            modifier = Modifier.fillMaxSize().padding(16.dp),
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            items(entries) { entry ->
                                HistoryRow(entry)
                                HorizontalDivider()
                            }
                        }
                    }
                }
            }
        }
    }
}

@androidx.compose.runtime.Composable
private fun HistoryRow(entry: AlertLog.Entry) {
    val time = runCatching {
        Instant.parse(entry.timestamp)
            .atZone(ZoneId.systemDefault())
            .format(TIME_FMT)
    }.getOrNull() ?: entry.timestamp

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            "$time · ${entry.alertType} · ${entry.urgency}",
            style = MaterialTheme.typography.labelMedium,
        )
        entry.app?.let {
            Text(it, style = MaterialTheme.typography.labelSmall)
        }
        Text(entry.quickMessage, style = MaterialTheme.typography.bodyMedium)
    }
}

private val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("dd MMM HH:mm")
