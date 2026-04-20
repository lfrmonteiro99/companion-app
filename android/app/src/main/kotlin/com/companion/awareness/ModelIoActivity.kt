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
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.json.JSONArray
import org.json.JSONObject
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * Human-readable view of every round-trip through [CoreBridge.analyze].
 * Each card shows a one-line summary (time, app, decision) and expands
 * to pretty-printed request + response JSON plus any captured error.
 */
class ModelIoActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ModelIoLog.init(this)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    val entries by ModelIoLog.entries.collectAsStateWithLifecycle()
                    // Newest first — easier to spot a fresh call after
                    // reproducing an issue.
                    val ordered = entries.asReversed()

                    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween,
                        ) {
                            Text(
                                "Model I/O (${entries.size})",
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Button(onClick = { ModelIoLog.clear(this@ModelIoActivity) }) {
                                Text("Clear")
                            }
                        }

                        if (entries.isEmpty()) {
                            Text(
                                "No calls yet. Start capture; each analyze call appears here.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        } else {
                            LazyColumn(
                                modifier = Modifier.fillMaxSize(),
                                verticalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                items(ordered) { entry -> IoCard(entry) }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun IoCard(entry: ModelIoLog.Entry) {
    var expanded by remember { mutableStateOf(false) }

    val time = runCatching {
        Instant.parse(entry.timestamp)
            .atZone(ZoneId.systemDefault())
            .format(TIME_FMT)
    }.getOrNull() ?: entry.timestamp

    val isError = entry.error != null
    val decisionText = when {
        isError -> "ERROR"
        entry.shouldAlert == true -> "ALERT · ${entry.alertType ?: "?"} · ${entry.urgency ?: "?"}"
        entry.shouldAlert == false -> "no alert"
        else -> "—"
    }
    val decisionColor = when {
        isError -> Color(0xFFC62828)
        entry.shouldAlert == true -> when (entry.urgency) {
            "high" -> Color(0xFFC62828)
            "medium" -> Color(0xFFEF6C00)
            else -> Color(0xFF2E7D32)
        }
        else -> Color(0xFF607D8B)
    }

    Card(
        modifier = Modifier.fillMaxWidth().clickable { expanded = !expanded },
        colors = CardDefaults.cardColors(),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    "$time · ${entry.durationMs} ms",
                    style = MaterialTheme.typography.labelMedium,
                )
                Text(
                    decisionText,
                    color = decisionColor,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
            entry.app?.let {
                Text(
                    it + (entry.windowTitle?.let { t -> " · $t" } ?: ""),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            if (entry.shouldAlert == true && !entry.quickMessage.isNullOrBlank()) {
                Text(entry.quickMessage, style = MaterialTheme.typography.bodyMedium)
            }
            if (isError) {
                Text(
                    entry.error!!,
                    color = Color(0xFFC62828),
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            if (expanded) {
                SectionLabel("Request")
                Text(
                    summarizeRequest(entry.requestJson),
                    style = MaterialTheme.typography.bodySmall,
                )
                Text(
                    prettyJson(entry.requestJson),
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF424242),
                )

                if (entry.responseJson != null) {
                    SectionLabel("Response")
                    Text(
                        prettyJson(entry.responseJson),
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF424242),
                    )
                }
            } else {
                Text(
                    "tap to expand request + response",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF607D8B),
                )
            }
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        modifier = Modifier.padding(top = 8.dp, bottom = 2.dp),
    )
}

/**
 * Pulls the interesting fields out of the request payload into a short
 * bullet block so the user doesn't have to read the raw JSON to see
 * what was actually sent.
 */
private fun summarizeRequest(requestJson: String): String {
    val o = runCatching { JSONObject(requestJson) }.getOrNull() ?: return ""
    val b = StringBuilder()
    o.optStringOrNull("app")?.let { b.appendLine("• app: $it") }
    o.optStringOrNull("window_title")?.let { b.appendLine("• window: $it") }
    val dur = o.optLong("duration_on_app_seconds", -1)
    if (dur >= 0) b.appendLine("• time on app: ${dur}s")
    o.optStringOrNull("mic_text_recent")?.let {
        b.appendLine("• mic: ${it.take(200)}")
    }
    o.optStringOrNull("screen_text_excerpt")?.let {
        b.appendLine("• screen (${it.length} chars): ${it.take(200).replace("\n", " ")}…")
    }
    val history = o.optJSONArray("history_apps_30min")
    if (history != null && history.length() > 0) {
        b.appendLine("• history: ${jsonArrayToList(history).joinToString(", ")}")
    }
    return b.toString().trimEnd()
}

private fun jsonArrayToList(a: JSONArray): List<String> =
    (0 until a.length()).map { a.optString(it, "") }

private fun prettyJson(raw: String): String =
    runCatching { JSONObject(raw).toString(2) }.getOrNull()
        ?: runCatching { JSONArray(raw).toString(2) }.getOrNull()
        ?: raw

private fun JSONObject.optStringOrNull(name: String): String? {
    if (isNull(name)) return null
    val s = optString(name, "")
    return s.ifBlank { null }
}

private val TIME_FMT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss")
