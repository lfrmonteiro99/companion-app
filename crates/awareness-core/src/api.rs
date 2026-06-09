use crate::config::Config;
use crate::types::{ContextEvent, FilterResponse};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ── Internal request / response structs ──────────────────────────────────────
//
// Targets Ollama's NATIVE `/api/chat` (not the `/v1/chat/completions`
// OpenAI-compat shim). The shim silently ignores `options.num_ctx`,
// which lets Ollama default to `num_ctx=8192` and forces ~14% of the
// 8B Q4 layers onto CPU on a 6 GB RTX 2060 — the smoke test caught this
// (qwen3:8b loaded at `14%/86% CPU/GPU 8192` on OMEN). The jarvis
// backend learned the same lesson; companion-app mirrors that choice.

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    /// Ollama structured-outputs: a JSON Schema object that grammar-constrains
    /// the model to emit exactly the FilterResponse fields (correct names,
    /// types, and `urgency` enum). This replaces the looser `format:"json"`,
    /// which let gemma3:4b invent field names (`alert_message` instead of
    /// `should_alert`) → parse failures → silent dropped alerts. See
    /// `filter_response_schema()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
    options: ChatOptions,
    /// How long Ollama keeps the model loaded in (V)RAM after this
    /// call. "30m" avoids paying ~25s of cold-load each time another
    /// process (e.g. jarvis) touches a different model.
    keep_alive: String,
    stream: bool,
}

#[derive(Serialize)]
struct ChatOptions {
    temperature: f32,
    /// Max generated tokens (Ollama-native name; OpenAI calls it
    /// `max_tokens`). Kept conservative — alerts are 20-30 words.
    num_predict: u32,
    /// Context window the model is loaded with. Must be set explicitly
    /// — without it Ollama defaults to 8192, which pushes the 8B Q4
    /// model partly to CPU on a 6 GB VRAM card. 6144 matches the jarvis
    /// default that's been verified GPU-only on the OMEN 2060.
    num_ctx: u32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    /// Optional so an Ollama error envelope (which carries no `message`)
    /// still deserializes instead of failing with a generic parse error.
    #[serde(default)]
    message: Option<ChatResponseMessage>,
    /// Ollama can answer HTTP 200 with `{"error": "..."}` (unknown model,
    /// model not loaded, OOM). Captured so the real cause is surfaced.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct FilterResponseRaw {
    should_alert: bool,
    alert_type: String,
    urgency: String,
    needs_deep_analysis: bool,
    quick_message: String,
    #[serde(default)]
    suggested_reply: Option<String>,
    #[serde(default)]
    suggested_action: Option<String>,
    /// Nested null-vs-object shape the model emits (see
    /// `filter_response_schema`); flattened into the public
    /// `FilterResponse.content_niche`/`content_theme` so the FFI/Kotlin
    /// surface is unchanged. The legacy flat fields are still accepted
    /// for old persisted JSON.
    #[serde(default)]
    content_idea: Option<ContentIdeaRaw>,
    #[serde(default)]
    content_niche: Option<String>,
    #[serde(default)]
    content_theme: Option<String>,
}

#[derive(Deserialize)]
struct ContentIdeaRaw {
    niche: String,
    theme: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// JSON Schema for the model's structured output (Ollama's `format` field).
///
/// Grammar-constrains generation to the exact `FilterResponseRaw` shape:
/// ALL nine fields are required — the four action/content fields stay
/// nullable but must be EMITTED. With them optional, gemma3:4b took the
/// grammar's shortest path and omitted the keys entirely (measured:
/// `content_niche=None` on a textbook pt_history seed, `suggested_reply`
/// populated only intermittently); required+nullable forces an explicit
/// value-or-null decision per field. `urgency` is a closed enum.
/// `alert_type` is a free string by design — the Android consumer treats it
/// as one (`optString` + prefix checks), so enum-locking it here could
/// forbid a legitimate value. Required-field locking is also what fixes the
/// silent-drop bug: gemma3:4b under plain `format:"json"` emitted
/// `alert_message` instead of `should_alert`, failing serde and dropping
/// the alert.
fn filter_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "should_alert": { "type": "boolean" },
            // Closed set mirroring the prompt's declared schema. Only
            // constrains MODEL output — core-synthesized values
            // ("skipped:*", "budget_exceeded", "duplicate",
            // "anti_interest", "error") never pass through the grammar.
            // Measured need: without it gemma3:4b labelled textbook
            // content-ideas as "focus", so the Gerar action never fired.
            "alert_type": {
                "type": "string",
                "enum": [
                    "focus", "time_spent", "emotional", "preparation",
                    "voice_reply", "content_idea", "none"
                ]
            },
            "urgency": { "type": "string", "enum": ["low", "medium", "high"] },
            "needs_deep_analysis": { "type": "boolean" },
            "quick_message": { "type": "string" },
            "suggested_reply": { "type": ["string", "null"] },
            "suggested_action": { "type": ["string", "null"] },
            // ONE null-vs-object decision instead of two independent
            // nullable strings. Iteration history on gemma3:4b: optional
            // fields → always omitted; required+nullable → invented
            // niches ("lifestyle"); +enum → force-picked
            // "portugal_history" for French/lifestyle content (small
            // models under grammar pressure avoid null on independent
            // fields). A single anyOf[null, object] makes "no idea" one
            // tiny completion, and inside the object the niche enum
            // still makes invention impossible.
            "content_idea": {
                "anyOf": [
                    { "type": "null" },
                    {
                        "type": "object",
                        "properties": {
                            "niche": {
                                "type": "string",
                                "enum": [
                                    "portugal_history",
                                    "portugal_alt_history",
                                    "ghost_stories_real"
                                ]
                            },
                            "theme": { "type": "string" }
                        },
                        "required": ["niche", "theme"]
                    }
                ]
            }
        },
        "required": [
            "should_alert",
            "alert_type",
            "urgency",
            "needs_deep_analysis",
            "quick_message",
            "suggested_reply",
            "suggested_action",
            "content_idea"
        ]
    })
}

/// Assemble the (system, user) turn contents for `filter_call`.
///
/// The system turn is BYTE-STABLE across every call — always exactly
/// `SYSTEM_PROMPT`. Ollama/llama.cpp prefix caching reuses the KV cache only
/// up to the first byte that differs between requests, so anything dynamic
/// prepended to the system turn forces a full ~2700-token re-prefill on every
/// tick (this was the previous behaviour: the user profile was prepended
/// here). The profile is semi-static — bio edits and rating-accumulated
/// interests change rarely — so it now LEADS the user turn: while unchanged,
/// the cached prefix extends across it too; when it changes, only the user
/// turn re-prefills. Truly per-tick content (history, matched interests,
/// event) stays at the tail.
fn build_turns(
    user_profile: &str,
    memory: &str,
    interests_line: &str,
    event_json: &str,
) -> (String, String) {
    let profile_block = if user_profile.trim().is_empty() {
        String::new()
    } else {
        format!(
            "PERFIL DO UTILIZADOR (prioriza isto ao decidir o que é relevante):\n{}\n\n---\n\n",
            user_profile.trim(),
        )
    };
    let user_content = if memory.is_empty() {
        format!("{profile_block}{interests_line}{event_json}")
    } else {
        format!(
            "{profile_block}Histórico recente (oldest first):\n{memory}\n\n{interests_line}Contexto actual:\n{event_json}",
        )
    };
    (SYSTEM_PROMPT.to_string(), user_content)
}

/// Derive Ollama's native chat endpoint, tolerating a trailing `/v1` or `/`.
pub(crate) fn ollama_chat_endpoint(base_url: &str) -> String {
    let root = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    format!("{}/api/chat", root)
}

/// Extract the first complete `{...}` JSON object from model output. Thinking
/// models may prepend reasoning or wrap JSON in markdown fences.
pub(crate) fn extract_json_object(s: &str) -> &str {
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => s,
    }
}

fn build_user_message(content: &str, image_png: Option<&[u8]>) -> ChatMessage {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let images = match image_png {
        Some(bytes) => vec![STANDARD.encode(bytes)],
        None => Vec::new(),
    };
    ChatMessage {
        role: "user".to_string(),
        content: content.to_string(),
        images,
    }
}

// ── System prompt ─────────────────────────────────────────────────────────────

// Restored 2026-05-29 to ~2700 tokens (was 3351 original, 969 trimmed).
// Real cost of the prompt is small now that we're on `/api/chat` native
// with num_ctx=6144 fully GPU-resident — prefill at ~1000 tok/s costs
// ~1s per 1000 prompt tokens. Cut only the 2 most specialised BOM/MAU
// examples (Instagram dev-tip + LinkedIn nova-posição) that didn't
// generalise; everything else that was driving quality is back. Worst
// case input (prompt + 8000-char screen + memory + output ≈ 5600 tok)
// still leaves comfortable headroom under num_ctx.
const SYSTEM_PROMPT: &str = r#"És um colega sénior que acompanha o ecrã do utilizador. Não és observador passivo — és alguém que ajuda, antecipa, e sugere acções concretas. Tens experiência em engenharia, comunicação profissional e debugging. Falas pouco, certeiro, em português europeu.

Recebes uma IMAGEM (screenshot da janela activa, possivelmente recortada à janela em foco) JUNTO com: TEXTO extraído (árvore de acessibilidade ou OCR), app detectada, transcrição recente do microfone, transcrição do áudio a tocar quando existir (campo media_audio_text), e um Histórico recente. A imagem é a fonte primária sobre o que está visualmente no ecrã (reels, vídeo, fotos, gráficos, layout); o texto extraído complementa-a e é mais fiável para texto denso de UI. Quando descreveres média (reel/vídeo/foto), baseia-te no que VÊS na imagem e no que se OUVE (media_audio_text), não só na legenda.

COMO USAR O HISTÓRICO RECENTE

O "Histórico recente" NÃO é o estado actual. São alertas que JÁ enviaste ao utilizador em ticks anteriores. Existe só para:
1. Evitar repetir — se a situação que vês no texto actual já foi descrita no histórico, responde should_alert=false. O utilizador já viu.
2. Detectar mudança — se o texto actual difere do histórico, podes alertar sobre o que MUDOU.

REGRAS DURAS sobre histórico:
- A tua resposta baseia-se só no TEXTO ACTUAL que recebeste. Nunca na memória.
- NUNCA escrevas quick_message a citar a memória (ex: "Luis mencionou que...", "Há X min tinhas...", "Continuas com..."). Proibido.
- Não inventes ficheiros, pessoas, ou erros que só viste no histórico.
- Se o texto actual não tem nada accionável, responde should_alert=false e descreve em 10 palavras o que lês agora, ponto final.

FORMATO DE quick_message

Cada quick_message tem duas partes numa só frase (ou duas curtas), 25-55 palavras:
1. EVIDÊNCIA — cita literalmente: nome da pessoa, texto exacto do erro, linha/função de código, palavras ditas, minutos decorridos. Zero paráfrases genéricas.
2. CONSELHO — acção concreta: resposta sugerida, causa provável + fix, melhoria de código, decisão de processo.

Sem emojis. Sem prefixos tipo "Nota:" ou "Aviso:". Escreve directo.

EXCEPÇÃO DE TAMANHO: em modo SCROLL/FEED SOCIAL (ver secção dedicada mais abaixo) o quick_message é prosa corrida 30-60 palavras, não 25-55. Todas as outras regras de forma (sem rótulos, sem emojis, directo, português europeu) continuam a aplicar-se.

EXEMPLOS

Teams com ping:
BOM: "João no Teams há 9 min: 'PR #142 pronto?' sem resposta. Sugestão: 'Ainda na review final, fecho antes das 18h.'"
MAU: "Teams aberto com conversa sobre pull request."

Código com anti-pattern:
BOM: "auth.rs linha 47: `.unwrap()` em Option<User> — panica se user não existir. Troca para `ok_or(AuthError::NotFound)?` e propaga."
MAU: "Ficheiro de código Rust aberto com funções de autenticação."

Terminal com erro:
BOM: "`npm install` falhou com EACCES em /usr/local/lib/node_modules. Evita sudo — usa nvm ou muda prefix: `npm config set prefix ~/.npm-global`."
MAU: "Terminal mostra erro de permissões."

Email com lista de ofertas de trabalho:
BOM: "Remote Rocketship: 'Senior Rust Engineer @ Figma — remoto EU — $180-220k'. Combina com o teu stack. Pensa: candidata hoje, ligação com o recruiter via LinkedIn nas próximas 48h."
MAU: "Email com lista de empregos remotos."

REGRAS PARA should_alert

Postura default: **SILÊNCIO**. A app NÃO narra nem comenta o que o utilizador já está a ver. should_alert=false é o default; should_alert=true é a excepção que tens de justificar.

REGRA DE OURO (sobrepõe-se a TUDO o resto neste prompt): só should_alert=true se a tua quick_message acrescenta pelo menos UMA destas quatro coisas que o ecrã NÃO dá:
  (1) CORRECÇÃO — algo factualmente errado e verificável que precisa de ser corrigido;
  (2) CONTEXTO NÃO-ÓBVIO — facto que muda o que o utilizador PENSARIA ou FARIA (uma decisão dele, uma ideia errada que ele tenha, uma consequência prática para ele). NÃO é trivia nem curiosidade sobre o TEMA do conteúdo: saberes mais sobre o ator, o filme, a marca, a receita ou o trend NÃO é valor — é mostrar conhecimento que ninguém pediu. "Acrescentar contexto" sobre um reel de entretenimento é quase sempre narração disfarçada → não o faças;
  (3) ACÇÃO — um próximo passo concreto a tomar;
  (4) SUGESTÃO — uma recomendação accionável.
TESTE DECISIVO: se removesses a parte de valor e sobrasse só uma descrição/resumo/paráfrase do que está visível → should_alert=false. Descrever o que o utilizador vê NÃO é valor. Frases como "Estás a ver um reel sobre X" / "Utilizador visualiza Y" / "Reel mostra Z" são PROIBIDAS. Assume sempre que o utilizador já viu o que está no ecrã.

should_alert=true sempre que consegues cumprir: "citar literalmente uma frase concreta do ecrã" + "acrescentar uma ligação, correcção, resposta ou próxima-acção concreta que o utilizador não teria de outra forma". Situações típicas:

- Pessoa à espera de resposta há tempo (cita pessoa, mensagem, minutos). Ver secção CHATS abaixo.
- Erro com causa legível no texto e fix plausível.
- Código com bug real ou anti-pattern visível e sugestão concreta.
- Evento iminente na agenda enquanto o utilizador faz outra coisa.
- Contradição entre apps ou mudança de contexto acidental.
- Sinal explícito de frustração (texto ou voz) com sugestão de próximo passo. Usa alert_type="emotional".
- **Post em rede social** (Reddit, X/Twitter, LinkedIn, Facebook, Instagram, Mastodon, HackerNews): aplica a REGRA DE OURO. Só alerta se tens uma CORRECÇÃO de algo errado OU CONTEXTO não-óbvio que o post não dá (contra-argumento concreto, dado técnico/histórico que muda a leitura) OU uma ACÇÃO/SUGESTÃO. O conteúdo ser "interessante", "viral" ou "substantivo" NÃO basta — isso não é valor, é entretenimento que o utilizador já está a consumir. alert_type="focus". NOTA: em apps de scroll (secção "CONTEÚDO DE SCROLL / FEED SOCIAL"), usa o FORMATO SCROLL.
- **Email ou notificação com proposta, oferta, convite** (entrevista, oferta de trabalho, projecto, evento, newsletter relevante à carreira). Cita o essencial (quem, o quê, prazo), avalia em 1 frase, sugere resposta concreta quando aplicável.
- **Artigo / doc / thread técnica** em que o conteúdo cruza com algo notável — aplicação prática, contraste com prática comum, truque não-óbvio, pegadilha. Ver INSIGHT abaixo.
- **Pergunta ou comando falado**: se mic_text_recent contém pergunta directa ("o que é X?", "como faço Y?") ou comando ("lembra-me de…", "resume isto"), RESPONDE em quick_message com alert_type="voice_reply". Cita a pergunta em 3-6 palavras e dá resposta concreta de 1-2 frases. Se não sabes responder com certeza, diz o que é preciso em vez de inventar.
- **Sinal emocional/stress só por voz**: se o tom em mic_text_recent indica frustração, confusão ou cansaço (sem keywords explícitas), alert_type="emotional". Cita a frase e propõe 1 passo concreto.
- **Facto objectivamente errado sobre coisa verificável publicamente** (datas históricas, figuras públicas, factos científicos, matemática, geografia, sintaxe técnica, APIs, nomes oficiais).

  REGRA FIRME: se o utilizador escreve afirmação factualmente errada, **should_alert=true**. alert_type="voice_reply". Cita literal o que escreveu + correcção numa frase. Exemplos:
    - "Hitler está vivo" → "Hitler morreu em 1945."
    - "a revolução dos cravos foi em 2025" → "A Revolução dos Cravos foi em 25 de Abril de 1974."
    - "o PI vale 3.2" → "π ≈ 3.14159."

  A forma em que está escrita NÃO te desobriga:
    - Declarativa, interrogativa retórica, rascunho não enviado, ou contida em parágrafo mais longo → alerta na mesma.

  Só NÃO alertes quando:
    - É opinião declarada como tal ("eu acho que X").
    - É hipótese explícita ("imagina que...", "e se...").
    - É ficção, sátira ou sarcasmo óbvio.
    - É citação atribuída a outros.
    - É detalhe privado não-verificável (endereços, agenda pessoal).
    - A tua confiança na correcção é <80%.

- **Insight / comentário proactivo** sobre conteúdo substantivo (artigo, doc técnica, livro, post com corpo, código não-trivial) onde consegues oferecer LIGAÇÃO CONCRETA que valha a pena partilhar. Não é paráfrase; é conhecimento adicional.

  should_alert=true, alert_type="focus". quick_message em 3 partes:
    1. **Observação**: cita literal a frase/ideia em 6-12 palavras.
    2. **Porque**: razão concreta da relevância — paralelo com outra ideia, contraste, contexto técnico, aplicação prática.
    3. **Pensa**: sugestão accionável em 1 frase.

  Exemplo: "Observação: 'async/await in Rust uses state machines compiled by the compiler.' Porque: explica por que Future precisa de Pin. Pensa: aplicar à Vec<Arc<Mutex<…>>> que rejeitaste há pouco — talvez Box::pin resolva."

  NÃO faças se: só tens paráfrase, ligação é trivialmente óbvia, texto é só chrome de UI, ou conteúdo não é substantivo.

should_alert=false SÓ nestes casos (lista fechada — na dúvida, alerta):
- Texto é só chrome de UI sem corpo (home screen, launcher, barra de sistema, lock screen, settings vazios).
- User está a começar a escrever algo sem substância ainda.
- **Anti-repetição dura**: se o Histórico recente já cobre a mesma página/PR/diff/erro/draft/mensagem/post, **should_alert=false obrigatório**. Aplica-se mesmo que: scroll mudou, novos comentários carregaram, timestamp varie, ou descrição esteja ligeiramente diferente. Só voltas a alertar quando um elemento central mudou realmente (PR diferente, frase factualmente diferente, mensagem de pessoa nova).
- **Apps de scroll/lazer** (Instagram, TikTok, YouTube, Reels, Twitter/X feed, Facebook feed, Pinterest, Threads, Bluesky feed): por defeito **should_alert=false**. Aplica a REGRA DE OURO sem excepções — só alertas se acrescentas CORRECÇÃO / CONTEXTO não-óbvio / ACÇÃO / SUGESTÃO, e essa parte de valor sobrevive sozinha sem descrever o reel. Um reel ser "interessante", "viral", "substantivo" ou cruzar com um interesse NÃO é, por si só, motivo para falar. Trivia ou "contexto" sobre o TEMA do reel (factos sobre o ator/filme/marca, origem do trend, dados curiosos) NÃO conta como valor — é entretenimento que o utilizador já está a consumir. Em entretenimento passivo, na prática só falas para **corrigir um erro factual claro** (ex: o reel afirma algo verificável e falso), ou se há uma **acção/ping directo** para o utilizador. Em todo o resto — incluindo reels sobre aparência/transformação de celebridades, lifestyle, humor — **should_alert=false**. O limiar é alto; na dúvida, **fica calado**.

quick_message continua obrigatório com should_alert=false. Sem conteúdo, descreve o estado em 10-15 palavras.

CHATS E MENSAGENS (Teams, Slack, WhatsApp, Discord, Signal, Messenger, Outlook, Gmail threads, comentários em PR/Jira)

Quando o user está a LER uma mensagem/email/comentário que alguém lhe enviou (e ainda não respondeu), o trabalho útil é **propor a resposta**.

1. Extrai do texto as linhas recentes (padrão `Nome [tempo]: texto`).
2. Identifica o dono do dispositivo (autor que se repete mais / próximo de "You", "Eu", "Sent from").
3. Encontra a ÚLTIMA mensagem que NÃO é dele, com timestamp recente.
4. Se ainda não respondeu → should_alert=true, alert_type="voice_reply".
5. quick_message TEM de incluir:
   - remetente + 3-6 palavras da mensagem dele,
   - **1-2 frases concretas de resposta sugerida**, em PT-EU, tom adequado ao contexto.

NÃO alertes em chats quando: última msg é do user (já respondeu), msg antiga sem novo ping, reacção/emoji/bot, grupo onde alguém já respondeu, texto insuficiente para saber quem escreveu, ou já alertaste sobre esta mesma mensagem.

STUCK: se Histórico mostra MESMA situação em 3+ entradas sem progresso, alerta com ABORDAGEM DIFERENTE — cita o elemento repetido e propõe novo próximo passo.

SCOPE & PR: se vês output de `git diff`/`git status` com muitos ficheiros + mensagem de commit/PR vaga ("wip", "fix", "update"), sugere título específico baseado no diff. Se o diff é maior do que o título sugere, recomenda dividir.

COMPOSE: se o user está a compor texto profissional (email, PR description, commit) com gralha óbvia, mistura PT/EN descontrolada, ou número/data errado face ao contexto → cita a parte errada + correcção. Só contextos profissionais claros.

MEETING PREP: se o texto mostra evento de calendar a começar em <15 min, alerta com assunto + hora + contexto relevante do Histórico (com quem estavas a falar do tema, ficheiro/PR relacionado aberto).

CONTEÚDO DE SCROLL / FEED SOCIAL

Quando o campo `app` é um dos pacotes abaixo E o texto do ecrã mostra um post/reel/vídeo individual (tem caption, descrição, título de vídeo, comentários visíveis — não é apenas chrome de feed ou listagem de thumbnails), NÃO uses o formato 3-partes "Observação/Porque/Pensa" do modo Insight. Usa o FORMATO SCROLL abaixo.

Pacotes de scroll:
  com.instagram.android, com.instagram.lite,
  com.zhiliaoapp.musically, com.ss.android.ugc.trill (TikTok),
  com.google.android.youtube (quando em Shorts),
  com.facebook.katana, com.facebook.lite,
  com.twitter.android, com.snapchat.android,
  com.pinterest, com.reddit.frontpage,
  com.linkedin.android (quando post individual, não feed vazio).
No desktop o `app` chega como nome amigável (ex: "Instagram", "TikTok", "YouTube", "Twitter/X", "Reddit") — aplica a mesma regra por substring case-insensitive.

FORMATO SCROLL (só quando a REGRA DE OURO autoriza falar; prosa corrida 30-60 palavras, sem rótulos, sem bullets, sem emojis, PT-EU directo):

LIDERA COM O VALOR — a correcção, o facto não-óbvio, ou a acção. NUNCA comeces por descrever ou resumir o reel; o utilizador já o viu. No máximo cita 3-5 palavras literais do reel só para ancorar a que te referes, e passa imediatamente ao que acrescentas. Se a tua frase, sem a parte de valor, é só uma descrição do reel → não a escrevas, fica calado.

EXEMPLO BOM (correcção, ~35 palavras): "O reel diz que 'só usamos 10% do cérebro' — é falso: exames mostram actividade em praticamente todas as regiões ao longo do dia. O mito vem de má interpretação de estudos antigos."
EXEMPLO BOM (silêncio): reel da transformação física de um ator, ou rotina matinal lo-fi — não tens correcção, facto não-óbvio, nem acção → should_alert=false. Não digas nada.
EXEMPLO MAU (PROIBIDO — é narração): "Reel viral de um pai a servir Coca-Cola e a juntar Jim Beam — formato dad-prank que circula desde 2022..." Isto descreve o que ele vê. Se a única substância é descrever o reel, fica calado.

Quando NÃO alertar em scroll: feed-chrome sem post aberto, listagem de thumbnails, loja/settings, DMs (caem em CHATS), e — sobretudo — sempre que a tua única mensagem possível seria descrever/resumir o reel. A REGRA DE OURO aplica-se primeiro.

alert_type fica "focus" para scroll. urgência é quase sempre "low".

CAMPOS OPCIONAIS — suggested_reply / suggested_action

Quando redijas uma resposta para o utilizador enviar (chats, voice_reply), põe essa resposta limpa — só o texto a enviar, sem aspas nem rótulos — em `suggested_reply` (e o quick_message mantém a evidência+conselho). Quando houver uma acção concreta a tomar, põe-na em `suggested_action`. Caso contrário, ambos `null`. NÃO repitas o quick_message nestes campos.

CANAIS DE CONTEÚDO (gatilho extra)

O utilizador tem 2 canais de Instagram onde gera vídeos a partir de história portuguesa. Quando o ecrã mostra material que serve DIRECTAMENTE de semente para um deles, PROPÕE uma ideia de conteúdo. Atenção: propor um vídeo a partir do que ele está a ler É o valor — NÃO é narrar o ecrã. Por isso, perante um facto/figura/evento histórico português claro, NÃO fiques calado por "estar só a descrever": propõe.

ROTEAMENTO (decide o canal por esta ordem, pára no primeiro que bate):
1. Há QUALQUER elemento sombrio/occulto — fantasma, assombração, cripta/mosteiro/lugar abandonado, relíquia/maldição, Inquisição, paranormal, morte misteriosa? → SEMPRE CANAL 2, niche "ghost_stories_real". MESMO que também seja história portuguesa (o sombrio ganha sempre ao histórico-normal).
2. Senão, é história/cultura/figura/evento/local PORTUGUÊS (não-sombrio)? → CANAL 1, niche "portugal_history" (ou "portugal_alt_history" se for claramente um cenário "e se" alternativo).
3. Senão (não-português, ou sem ângulo concreto) → não proponhas: content_idea = null.

Quando disparas: alert_type = "content_idea", urgency = "low", content_idea = {"niche": a key exacta, "theme": tópico-semente conciso em PT-EU (uma frase)}, quick_message = "Isto dava um vídeo: <ângulo concreto>".

EXEMPLOS:
- "Batalha de Aljubarrota (1385), D. João I derrota Castela" → CANAL 1. content_idea={"niche":"portugal_history","theme":"A Batalha de Aljubarrota e como garantiu a independência de Portugal"}, quick_message="Isto dava um vídeo: Aljubarrota, a batalha que salvou a independência de Portugal em 1385."
- "Convento abandonado, relatos de aparições de monges" → CANAL 2 (tem assombração). content_idea={"niche":"ghost_stories_real","theme":"O convento abandonado e os relatos de aparições de monges"}.
- "The French Revolution, 1789" → content_idea=null (não é português, sem elemento sombrio).
- Reel de rotina matinal / lifestyle / humor → content_idea=null E fica calado.

Não forces: se não é português E não é claramente sombrio, content_idea=null — NUNCA encaixes conteúdo estrangeiro ou genérico num dos nichos. Em scroll de entretenimento sem matéria histórica/sombria → fica calado.

URGENCY:
- "high" — prazo imediato (reunião a começar, crash bloqueante, deadline a estourar).
- "medium" — default para erros accionáveis e pings à espera.
- "low" — sugestões de melhoria, observações com conselho sem pressão.

Responde SEMPRE JSON válido neste schema exacto:
{
  "should_alert": boolean,
  "alert_type": "focus" | "time_spent" | "emotional" | "preparation" | "voice_reply" | "content_idea" | "none",
  "urgency": "low" | "medium" | "high",
  "needs_deep_analysis": boolean,
  "quick_message": string,
  "suggested_reply": string | null,
  "suggested_action": string | null,
  "content_idea": { "niche": "portugal_history" | "portugal_alt_history" | "ghost_stories_real", "theme": string } | null
}

As 3 chaves anuláveis (suggested_reply, suggested_action, content_idea) são SEMPRE emitidas — escreve null quando não se aplicam, e null é a resposta CERTA na esmagadora maioria dos ticks. content_idea só deixa de ser null quando o ecrã é semente DIRECTA para um dos 2 canais do utilizador (ver CANAIS DE CONTEÚDO): história/cultura PORTUGUESA ou tema sombrio/assombrado. Revolução Francesa, rotinas matinais, lifestyle, tech → content_idea=null, sem excepção. Preencher content_idea NÃO é, por si, motivo para alertar: should_alert rege-se exclusivamente pela REGRA DE OURO. FORMATO OBRIGATÓRIO: sempre que content_idea NÃO é null, alert_type="content_idea" E o quick_message começa exactamente por "Isto dava um vídeo:" — sem estas duas marcas a proposta é inválida."#;

// ── Client ────────────────────────────────────────────────────────────────────

/// Local-LLM client over the Ollama OpenAI-compatible chat endpoint.
///
/// Type name kept as `OpenAiClient` so the Android JNI bridge in
/// `android/core-rs` keeps linking without changes. The struct now
/// targets Ollama at `cfg.llm_base_url` (default OMEN over Tailscale,
/// `http://100.68.73.123:11434/v1`) and sends no `Authorization` header
/// when `api_key` is empty.
#[derive(Clone)]
pub struct OpenAiClient {
    http: Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAiClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        warn_if_insecure_endpoint(&cfg.llm_base_url);
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.llm_timeout_seconds))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            base_url: cfg.llm_base_url.clone(),
            model: cfg.llm_model.clone(),
            api_key: cfg.openai_api_key.clone(),
        })
    }

    /// Build an `OpenAiClient` from an API key only. Retained as a fallback
    /// constructor; defaults are sourced from the shared `DEFAULT_*` constants
    /// so this can never silently diverge from `Config` again. (It previously
    /// hardcoded `qwen3:8b` independently of `DEFAULT_LLM_MODEL`, so changing
    /// the config default did nothing on the path that used this — the exact
    /// trap that pinned the Android app to the slow thinking model.) The
    /// Android bridge now builds via `new(&Config)`, making config authoritative.
    pub fn with_api_key(api_key: String) -> Result<Self> {
        let base_url = crate::config::DEFAULT_LLM_BASE_URL.to_string();
        warn_if_insecure_endpoint(&base_url);
        let http = Client::builder()
            .timeout(Duration::from_secs(
                crate::config::DEFAULT_LLM_TIMEOUT_SECONDS,
            ))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            base_url,
            model: crate::config::DEFAULT_LLM_MODEL.to_string(),
            api_key,
        })
    }

    pub async fn filter_call(
        &self,
        event: &ContextEvent,
        memory: &str,
        user_profile: &str,
        matched_interests: &[String],
        image_png: Option<&[u8]>,
    ) -> Result<FilterResponse> {
        let event_json =
            serde_json::to_string(event).context("failed to serialise ContextEvent")?;
        // Build the user turn. Matched-interests line is dynamic per
        // tick, so it belongs here rather than in the static system
        // prompt (keeps future prompt caching friendly). Sort and cap
        // the list inside the formatter for predictable token count.
        let interests_line = if matched_interests.is_empty() {
            String::new()
        } else {
            format!(
                "Interesses do utilizador que aparecem no ecrã actual (comenta/resume com prioridade): {}\n\n",
                matched_interests.join(", "),
            )
        };
        let (system_content, user_content) =
            build_turns(user_profile, memory, &interests_line, &event_json);

        // Both the text and vision paths run gemma3:4b — a non-thinking
        // instruction-follower — and both use the SAME structured-outputs
        // schema (filter_response_schema). gemma3:4b honours the JSON-Schema
        // grammar with or without an image attached, so we get correct field
        // names in one shot. `num_predict` stays small: there is no thinking
        // budget to burn, and a complete FilterResponse is ~60-150 tokens;
        // 256 leaves headroom for the longest SCROLL message without risking
        // a mid-JSON truncation (which would parse-fail → drop the alert).
        // (Thinking VL models such as qwen3-vl remain unsupported: under a
        // grammar they emit empty output and blow past the generation budget.)
        let vision = image_png.is_some();
        let format = Some(filter_response_schema());
        let num_predict = 256u32;
        let num_ctx = if vision { 8192u32 } else { 6144u32 };

        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_content,
                    images: Vec::new(),
                },
                build_user_message(&user_content, image_png),
            ],
            format,
            options: ChatOptions {
                temperature: 0.3,
                num_predict,
                num_ctx,
            },
            keep_alive: "30m".to_string(),
            stream: false,
        };

        // Retry up to 2 extra attempts (3 total) with exponential back-off on
        // 429 / 5xx responses.
        let backoff_ms: [u64; 2] = [500, 1500];
        let mut last_err: Option<anyhow::Error> = None;

        'retry: for attempt in 0..=2usize {
            if attempt > 0 {
                let wait = backoff_ms[attempt - 1];
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }

            let url = ollama_chat_endpoint(&self.base_url);
            let mut req = self.http.post(&url).json(&body);
            if !self.api_key.is_empty() {
                req = req.bearer_auth(&self.api_key);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e.into());
                    continue 'retry;
                }
            };

            let status = resp.status();

            // Retryable errors
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = Some(anyhow::anyhow!("LLM returned status {}", status));
                continue 'retry;
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("LLM error {}: {}", status, text);
            }

            let chat: ChatResponse = resp
                .json()
                .await
                .context("failed to deserialise ChatResponse")?;

            // A 200 response carrying an error envelope (wrong model name,
            // etc.) is not retryable — surface the real cause instead of a
            // confusing "failed to deserialise" further down.
            if let Some(err) = chat.error {
                anyhow::bail!("Ollama returned error: {err}");
            }
            let message = chat
                .message
                .context("Ollama response had neither `message` nor `error`")?;

            let tokens_in = chat.prompt_eval_count;
            let tokens_out = chat.eval_count;
            // Local LLM is free at runtime; cost stays zero so the
            // budget controller is a no-op without ripping it out.
            let cost_usd = 0.0;

            let raw_content = message.content;
            let json_slice = extract_json_object(&raw_content);

            let raw: FilterResponseRaw = match serde_json::from_str(json_slice) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        "api: failed to parse FilterResponse JSON ({}): {:?}",
                        e,
                        raw_content
                    );
                    return Ok(FilterResponse {
                        should_alert: false,
                        alert_type: "none".to_string(),
                        urgency: "low".to_string(),
                        needs_deep_analysis: false,
                        quick_message: String::new(),
                        suggested_reply: None,
                        suggested_action: None,
                        content_niche: None,
                        content_theme: None,
                        tokens_in,
                        tokens_out,
                        cost_usd,
                        parse_error: Some(e.to_string()),
                        matched_interests: matched_interests.to_vec(),
                    });
                }
            };

            // Flatten the nested content_idea (current schema) with the
            // legacy flat fields as fallback for old persisted JSON.
            let (content_niche, content_theme) = match raw.content_idea {
                Some(idea) => (Some(idea.niche), Some(idea.theme)),
                None => (raw.content_niche, raw.content_theme),
            };
            return Ok(FilterResponse {
                should_alert: raw.should_alert,
                alert_type: raw.alert_type,
                urgency: raw.urgency,
                needs_deep_analysis: raw.needs_deep_analysis,
                quick_message: raw.quick_message,
                suggested_reply: raw.suggested_reply,
                suggested_action: raw.suggested_action,
                content_niche,
                content_theme,
                tokens_in,
                tokens_out,
                cost_usd,
                parse_error: None,
                matched_interests: matched_interests.to_vec(),
            });
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all retries exhausted")))
    }
}

/// Log a warning when sensitive screen/mic text (and any bearer token)
/// would be sent to a non-local endpoint over plaintext HTTP. Loopback
/// and Tailscale (100.64.0.0/10 CGNAT) hosts are exempt — they're the
/// intended transport. Never refuses; a warning only, so existing setups
/// keep working unchanged.
fn warn_if_insecure_endpoint(base_url: &str) {
    let url = base_url.trim();
    let Some(rest) = url.strip_prefix("http://") else {
        return; // https or another scheme — not plaintext.
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    let is_local = host == "localhost"
        || host == "[::1]"
        || host.starts_with("127.")
        || is_tailscale_cgnat(host);
    if !is_local {
        tracing::warn!(
            "llm_base_url uses plaintext http:// to a non-local host ({host}); \
             screen/mic text and any API key are sent unencrypted. Prefer https \
             or a loopback/Tailscale endpoint."
        );
    }
}

/// True for IPv4 literals in 100.64.0.0/10 (Tailscale / CGNAT range).
fn is_tailscale_cgnat(host: &str) -> bool {
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() != 4 || !octets.iter().all(|o| o.parse::<u8>().is_ok()) {
        return false;
    }
    match (octets[0].parse::<u8>(), octets[1].parse::<u8>()) {
        (Ok(100), Ok(second)) => (64..=127).contains(&second),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_turn_is_byte_stable_regardless_of_profile() {
        // Prefix caching reuses KV only up to the first differing byte —
        // the system turn must be EXACTLY the same bytes on every call,
        // profile or no profile. A regression here silently costs a full
        // ~2700-token re-prefill per tick.
        let (sys_empty, _) = build_turns("", "history", "", "{}");
        let (sys_full, _) = build_turns("bio: dev", "history", "interests\n\n", "{}");
        assert_eq!(sys_empty, SYSTEM_PROMPT);
        assert_eq!(sys_full, SYSTEM_PROMPT);
    }

    #[test]
    fn profile_leads_user_turn_then_history_then_event() {
        let (_, user) = build_turns("bio: dev", "old alert", "interesse\n\n", "{\"app\":\"x\"}");
        let p = user.find("PERFIL DO UTILIZADOR").expect("profile present");
        let h = user.find("Histórico recente").expect("history present");
        let e = user.find("{\"app\":\"x\"}").expect("event present");
        assert!(p < h && h < e, "order must be profile < history < event");
    }

    #[test]
    fn empty_profile_and_memory_user_turn_is_just_interests_and_event() {
        let (_, user) = build_turns("", "", "", "{\"app\":\"x\"}");
        assert_eq!(user, "{\"app\":\"x\"}");
        assert!(!user.contains("PERFIL"));
        assert!(!user.contains("Histórico"));
    }

    fn sample(parse_error: Option<String>) -> FilterResponse {
        FilterResponse {
            should_alert: false,
            alert_type: "none".into(),
            urgency: "low".into(),
            needs_deep_analysis: false,
            quick_message: String::new(),
            suggested_reply: None,
            suggested_action: None,
            content_niche: None,
            content_theme: None,
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: 0.000018,
            parse_error,
            matched_interests: Vec::new(),
        }
    }

    #[test]
    fn ollama_chat_endpoint_strips_v1() {
        assert_eq!(
            ollama_chat_endpoint("http://omen:11434/v1"),
            "http://omen:11434/api/chat"
        );
        assert_eq!(
            ollama_chat_endpoint("http://localhost:11434/"),
            "http://localhost:11434/api/chat"
        );
    }

    #[test]
    fn extract_json_object_handles_thinking_prefix_and_fences() {
        let s = "Okay, let me think... \n```json\n{\"should_alert\": true}\n```";
        assert_eq!(extract_json_object(s), "{\"should_alert\": true}");
        let plain = "{\"a\":1}";
        assert_eq!(extract_json_object(plain), plain);
    }

    #[test]
    fn build_user_message_includes_base64_image_when_present() {
        let bytes = vec![1u8, 2, 3, 250];
        let msg = build_user_message("hi", Some(&bytes));
        let v = serde_json::to_value(&msg).unwrap();
        let b64 = v["images"][0].as_str().unwrap();
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        assert_eq!(STANDARD.decode(b64).unwrap(), bytes);
        // No image → no images field serialized.
        let msg2 = build_user_message("hi", None);
        let v2 = serde_json::to_value(&msg2).unwrap();
        assert!(
            v2.get("images").is_none(),
            "images must be omitted when absent"
        );
    }

    #[test]
    fn system_prompt_states_it_receives_an_image() {
        assert!(SYSTEM_PROMPT.contains("IMAGEM"));
        assert!(SYSTEM_PROMPT.contains("media_audio_text"));
        assert!(!SYSTEM_PROMPT.contains("Não recebes imagem"));
    }

    #[test]
    fn system_prompt_contains_suggested_reply_schema() {
        assert!(
            SYSTEM_PROMPT.contains("suggested_reply"),
            "SYSTEM_PROMPT must instruct the model to populate suggested_reply"
        );
        assert!(
            SYSTEM_PROMPT.contains("suggested_action"),
            "SYSTEM_PROMPT must instruct the model to populate suggested_action"
        );
    }

    #[test]
    fn system_prompt_contains_content_channels_section() {
        assert!(
            SYSTEM_PROMPT.contains("CANAIS DE CONTEÚDO"),
            "SYSTEM_PROMPT must contain the content channels section"
        );
        // Schema shape changed 2026-06-09: the model now emits ONE
        // nullable object — content_idea: {niche, theme} | null — instead
        // of two independent nullable strings (gemma3:4b avoided null on
        // independent fields and junk-filled them). The prompt must teach
        // the nested keys and the mandatory proposal format.
        assert!(
            SYSTEM_PROMPT.contains("\"niche\""),
            "SYSTEM_PROMPT must reference the nested niche key in schema"
        );
        assert!(
            SYSTEM_PROMPT.contains("\"theme\""),
            "SYSTEM_PROMPT must reference the nested theme key in schema"
        );
        assert!(
            SYSTEM_PROMPT.contains("Isto dava um vídeo:"),
            "SYSTEM_PROMPT must mandate the canonical proposal phrase (the Kotlin Gerar gate keys on it)"
        );
        assert!(
            SYSTEM_PROMPT.contains("content_idea"),
            "SYSTEM_PROMPT must include content_idea in alert_type enum"
        );
    }

    #[test]
    fn parse_error_absent_when_none() {
        let r = sample(None);
        let s = serde_json::to_string(&r).unwrap();
        // skip_serializing_if = "Option::is_none" must drop the field entirely.
        assert!(
            !s.contains("parse_error"),
            "serialized JSON should omit parse_error: {s}"
        );
    }

    #[test]
    fn parse_error_present_when_some() {
        let r = sample(Some("schema mismatch".into()));
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            s.contains("parse_error"),
            "JSON must contain parse_error field: {s}"
        );
        assert!(s.contains("schema mismatch"));
    }

    #[test]
    fn parse_error_round_trips() {
        let r = sample(Some("boom".into()));
        let s = serde_json::to_string(&r).unwrap();
        let back: FilterResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back.parse_error.as_deref(), Some("boom"));
        assert_eq!(back.tokens_in, 10);
        assert_eq!(back.tokens_out, 20);
    }

    #[test]
    fn chat_response_parses_ollama_error_envelope() {
        // HTTP 200 with an error body must deserialize (message optional)
        // so we can surface the real cause rather than a parse failure.
        let body = r#"{"error":"model 'qwen3:8b' not found, try pulling it first"}"#;
        let parsed: ChatResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.message.is_none());
        assert_eq!(
            parsed.error.as_deref(),
            Some("model 'qwen3:8b' not found, try pulling it first")
        );
    }

    #[test]
    fn chat_response_parses_normal_message() {
        let body = r#"{"message":{"content":"{}"},"prompt_eval_count":5,"eval_count":6}"#;
        let parsed: ChatResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.message.unwrap().content, "{}");
        assert_eq!(parsed.prompt_eval_count, 5);
        assert_eq!(parsed.eval_count, 6);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn cgnat_and_local_hosts_are_recognised() {
        assert!(is_tailscale_cgnat("100.68.73.123")); // the OMEN default
        assert!(is_tailscale_cgnat("100.64.0.1"));
        assert!(is_tailscale_cgnat("100.127.255.255"));
        assert!(!is_tailscale_cgnat("100.128.0.1")); // just outside /10
        assert!(!is_tailscale_cgnat("100.63.0.1"));
        assert!(!is_tailscale_cgnat("8.8.8.8"));
        assert!(!is_tailscale_cgnat("not.an.ip"));
    }

    #[test]
    fn with_api_key_builds_client_without_full_config() {
        // Android frontend path: no Config struct, just an API key.
        let client = OpenAiClient::with_api_key("sk-dummy".into()).expect("client must build");
        assert_eq!(client.api_key, "sk-dummy");
    }

    #[test]
    fn deserializing_legacy_json_without_parse_error_is_ok() {
        // JSONL persisted by previous versions won't carry parse_error —
        // the #[serde(default)] must keep old logs readable.
        let legacy = r#"{
          "should_alert": true,
          "alert_type": "emotional",
          "urgency": "high",
          "needs_deep_analysis": false,
          "quick_message": "oops",
          "tokens_in": 5,
          "tokens_out": 6,
          "cost_usd": 0.0001
        }"#;
        let r: FilterResponse = serde_json::from_str(legacy).unwrap();
        assert_eq!(r.parse_error, None);
        assert!(r.should_alert);
    }
}
