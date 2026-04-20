package com.companion.awareness

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.InputChip
import androidx.compose.material3.InputChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateList
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import org.json.JSONArray
import org.json.JSONObject

/**
 * Profile editor with three panels:
 *   1. Free-text bio (goes verbatim into the system prompt).
 *   2. Curated interest pills — user adds from a ~150-item catalogue
 *      or types free-form. Each tick filters these down to the ones
 *      actually matching the screen, so only ~10 tokens are spent per
 *      API call on interest context.
 *   3. Read-only view of interests/anti-interests the rating buttons
 *      on notifications have accumulated + top apps observed.
 */
class ProfileActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    var bio by remember { mutableStateOf(Settings.userBio(this@ProfileActivity)) }
                    val interests = remember {
                        mutableStateListOf<String>().apply {
                            addAll(Settings.explicitInterests(this@ProfileActivity))
                        }
                    }
                    var summary by remember { mutableStateOf("{}") }

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
                        Text("Profile", style = MaterialTheme.typography.titleLarge)

                        // ── Bio ──────────────────────────────────────
                        Text(
                            "O que escreveres aqui é injectado no system prompt do modelo. " +
                                "Descreve quem és, o teu trabalho, o que queres que a app observe.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        OutlinedTextField(
                            value = bio,
                            onValueChange = { bio = it },
                            label = { Text("Sobre ti") },
                            modifier = Modifier.fillMaxWidth(),
                            minLines = 5,
                        )
                        Button(onClick = {
                            Settings.setUserBio(this@ProfileActivity, bio)
                            runCatching { CoreBridge.setBio(bio) }
                            summary = runCatching { CoreBridge.getProfileText() }.getOrDefault("{}")
                        }) {
                            Text("Guardar bio")
                        }

                        HorizontalDivider()

                        // ── Interests (explicit) ─────────────────────
                        InterestsEditor(
                            interests = interests,
                            onChanged = {
                                Settings.setExplicitInterests(
                                    this@ProfileActivity,
                                    interests.toList(),
                                )
                                runCatching {
                                    CoreBridge.setExplicitInterests(interests.toTypedArray())
                                }
                            },
                        )

                        HorizontalDivider()

                        // ── Learned (read-only) ──────────────────────
                        Text(
                            "Aprendido automaticamente das notificações (botões 'Mais disto' / 'Não interessa')",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        LearnedSummary(summary)
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)
@Composable
private fun InterestsEditor(
    interests: SnapshotStateList<String>,
    onChanged: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    val keyboard = LocalSoftwareKeyboardController.current

    // derivedStateOf so the filter is only recomputed when the query
    // or the selected set changes. 40 items max — enough for the
    // user to scan, keeps the dropdown from being a wall of text.
    val suggestions by remember(interests) {
        derivedStateOf {
            val q = query.trim().lowercase()
            val selected = interests.map { it.lowercase() }.toSet()
            InterestsCatalog.ALL
                .asSequence()
                .filter { it.lowercase() !in selected }
                .filter { q.isEmpty() || q in it.lowercase() }
                .take(40)
                .toList()
        }
    }
    val customAddPossible by remember(interests) {
        derivedStateOf {
            val t = query.trim()
            t.length >= 3 &&
                interests.none { it.equals(t, ignoreCase = true) } &&
                InterestsCatalog.ALL.none { it.equals(t, ignoreCase = true) }
        }
    }

    fun commit(raw: String) {
        val t = raw.trim()
        if (t.length < 3) return
        if (interests.any { it.equals(t, ignoreCase = true) }) return
        interests.add(t)
        query = ""
        onChanged()
    }

    Text("Os teus interesses", style = MaterialTheme.typography.titleSmall)
    Text(
        "Escolhe da lista ou escreve e carrega Enter. A app só envia os que aparecem no ecrã — mantém o custo baixo.",
        style = MaterialTheme.typography.bodySmall,
    )

    if (interests.isNotEmpty()) {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            interests.forEach { label ->
                InputChip(
                    selected = true,
                    onClick = { },
                    label = { Text(label, maxLines = 1) },
                    trailingIcon = {
                        Icon(
                            Icons.Default.Close,
                            contentDescription = "Remover $label",
                            modifier = Modifier
                                .clickable {
                                    interests.remove(label)
                                    onChanged()
                                },
                        )
                    },
                    colors = InputChipDefaults.inputChipColors(),
                )
            }
        }
    }

    OutlinedTextField(
        value = query,
        onValueChange = { if (it.length <= 80) query = it },
        label = { Text("Adicionar interesse") },
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = {
            commit(query)
            keyboard?.hide()
        }),
    )

    // Suggestion list: inline (not a popup dropdown) so small screens
    // can scroll everything together. Bounded height so it doesn't
    // push the rest of the profile off-screen.
    if (suggestions.isNotEmpty() || customAddPossible) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 240.dp),
        ) {
            LazyColumn(modifier = Modifier.fillMaxWidth()) {
                if (customAddPossible) {
                    item {
                        Text(
                            "+ Adicionar \"${query.trim()}\"",
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { commit(query) }
                                .padding(vertical = 10.dp, horizontal = 8.dp),
                        )
                    }
                }
                items(suggestions, key = { it }) { tag ->
                    Text(
                        tag,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { commit(tag) }
                            .padding(vertical = 10.dp, horizontal = 8.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun LearnedSummary(json: String) {
    val obj = runCatching { JSONObject(json) }.getOrNull()
    if (obj == null) {
        Text("—", style = MaterialTheme.typography.bodySmall)
        return
    }
    val interests = obj.optJSONArray("interests")
    val antiInterests = obj.optJSONArray("anti_interests")
    val topApps = obj.optString("top_apps", "")

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        sectionIfNonEmpty("Mais disto:", interests)
        sectionIfNonEmpty("Não interessa:", antiInterests)
        if (topApps.isNotBlank()) {
            Text(topApps, style = MaterialTheme.typography.bodySmall)
        }
        if ((interests?.length() ?: 0) == 0 &&
            (antiInterests?.length() ?: 0) == 0 &&
            topApps.isBlank()
        ) {
            Text(
                "Sem dados ainda. Usa a app uns minutos e carrega nos botões das notificações.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun sectionIfNonEmpty(header: String, arr: JSONArray?) {
    if (arr == null || arr.length() == 0) return
    Text(header, style = MaterialTheme.typography.labelMedium)
    for (i in 0 until arr.length()) {
        Text("• ${arr.optString(i)}", style = MaterialTheme.typography.bodySmall)
    }
}
