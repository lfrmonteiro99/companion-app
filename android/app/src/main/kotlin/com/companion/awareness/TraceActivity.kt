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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

class TraceActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var tick by remember { mutableStateOf(0) }
                    val entries = remember(tick) { TraceLog.recent(this@TraceActivity) }

                    // Auto-refresh every 3s so the screen feels live while
                    // capture runs. Stops when the user leaves the activity.
                    LaunchedEffect(Unit) {
                        while (true) {
                            delay(3_000)
                            tick++
                        }
                    }

                    Column(modifier = Modifier.fillMaxSize().padding(12.dp)) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Text(
                                "Pipeline trace (${entries.size})",
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Box(modifier = Modifier.weight(1f))
                            OutlinedButton(onClick = { tick++ }) { Text("Refresh") }
                            Button(onClick = {
                                TraceLog.clear(this@TraceActivity)
                                tick++
                            }) { Text("Clear") }
                        }
                        if (entries.isEmpty()) {
                            Box(
                                modifier = Modifier.fillMaxSize(),
                                contentAlignment = Alignment.Center,
                            ) {
                                Text(
                                    "Nothing yet. Start capture and leave it running for 10 seconds.",
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                            }
                        } else {
                            LazyColumn(
                                modifier = Modifier.fillMaxSize(),
                                verticalArrangement = Arrangement.spacedBy(6.dp),
                            ) {
                                items(entries) { entry ->
                                    TraceRow(entry)
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
private fun TraceRow(entry: TraceLog.Entry) {
    val time = runCatching {
        Instant.parse(entry.timestamp)
            .atZone(ZoneId.systemDefault())
            .format(TIME_FMT)
    }.getOrNull() ?: entry.timestamp

    val (label, accent) = when (entry.stage) {
        TraceLog.Stage.CAPTURE -> "CAPTURE" to Color(0xFF5C6BC0)
        TraceLog.Stage.GATE_SKIP -> "GATE SKIP" to Color(0xFF757575)
        TraceLog.Stage.GATE_SEND -> "GATE SEND" to Color(0xFF2E7D32)
        TraceLog.Stage.API_RESP -> "API" to Color(0xFF1565C0)
        TraceLog.Stage.BUDGET -> "BUDGET" to Color(0xFFB8860B)
        TraceLog.Stage.NOTIFY_POST -> "NOTIFY" to Color(0xFF8E24AA)
        TraceLog.Stage.NOTIFY_SUPPRESS -> "NOTIFY SKIP" to Color(0xFF607D8B)
        TraceLog.Stage.ANALYZE_FAIL -> "ERROR" to Color(0xFFCC3333)
    }

    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                label,
                style = MaterialTheme.typography.labelSmall.copy(
                    fontWeight = FontWeight.Bold,
                    color = Color.White,
                ),
                modifier = Modifier
                    .background(accent)
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            )
            Text(
                "$time · tick #${entry.tickId}",
                style = MaterialTheme.typography.labelMedium,
            )
        }
        Text(entry.text, style = MaterialTheme.typography.bodySmall)
    }
}

private val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss")
