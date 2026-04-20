package com.companion.awareness

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * Live view of the in-app diagnostic log. Includes uncaught exception
 * stack traces captured by [DebugLog.installCrashHandler]. Most recent
 * entries at the bottom; auto-scrolls to the latest.
 */
class LogsActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        DebugLog.init(this)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    val entries by DebugLog.entries.collectAsStateWithLifecycle()
                    val listState = rememberLazyListState()

                    LaunchedEffect(entries.size) {
                        if (entries.isNotEmpty()) {
                            listState.animateScrollToItem(entries.size - 1)
                        }
                    }

                    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween,
                        ) {
                            Text(
                                "Logs (${entries.size})",
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Button(onClick = { DebugLog.clear(this@LogsActivity) }) {
                                Text("Clear")
                            }
                        }

                        if (entries.isEmpty()) {
                            Text(
                                "No logs yet. Start capture; entries appear here in real time.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        } else {
                            LazyColumn(
                                state = listState,
                                modifier = Modifier.fillMaxSize(),
                                verticalArrangement = Arrangement.spacedBy(6.dp),
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
private fun LogRow(entry: DebugLog.Entry) {
    var expanded by remember { mutableStateOf(false) }
    val hasStack = entry.stack != null

    val time = runCatching {
        Instant.parse(entry.timestamp)
            .atZone(ZoneId.systemDefault())
            .format(TIME_FMT)
    }.getOrNull() ?: entry.timestamp

    val levelColor = when (entry.level) {
        DebugLog.Level.D -> Color(0xFF607D8B)
        DebugLog.Level.I -> Color(0xFF2E7D32)
        DebugLog.Level.W -> Color(0xFFEF6C00)
        DebugLog.Level.E -> Color(0xFFC62828)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .let { if (hasStack) it.clickable { expanded = !expanded } else it }
            .padding(vertical = 2.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                entry.level.name,
                color = levelColor,
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.padding(end = 8.dp),
            )
            Text(
                "$time · ${entry.tag}",
                style = MaterialTheme.typography.labelSmall,
            )
        }
        Text(entry.message, style = MaterialTheme.typography.bodySmall)
        if (hasStack && expanded) {
            Text(
                entry.stack!!,
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFF8B0000),
            )
        } else if (hasStack) {
            Text(
                "tap to expand stack trace",
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFF8B0000),
            )
        }
    }
}

private val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss.SSS")
