use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

use crate::config::Config;
use crate::types::{ContextEvent, FilterResponse};

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatMessage {
    System {
        role: &'static str,
        content: String,
    },
    UserMulti {
        role: &'static str,
        content: Vec<ContentPart>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
    /// "low" | "high" | "auto". Low = ~85 input tokens/image.
    /// Auto/high tokenises the image at full resolution and costs far more.
    detail: &'static str,
}

#[derive(Serialize)]
struct ResponseFormat {
    r#type: String,
}

// ── Response structs (shared shape with text backend) ────────────────────────

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: MessageContent,
}

#[derive(serde::Deserialize)]
struct MessageContent {
    content: String,
}

#[derive(serde::Deserialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[derive(serde::Deserialize)]
struct FilterResponseRaw {
    should_alert: bool,
    alert_type: String,
    urgency: String,
    needs_deep_analysis: bool,
    quick_message: String,
}

// ── Vision system prompt ─────────────────────────────────────────────────────

const VISION_SYSTEM_PROMPT: &str = r#"És um colega sénior a olhar para o ecrã do utilizador. Não és observador passivo a descrever o que se vê — és alguém que ajuda, antecipa, e sugere acções concretas. Tens experiência em engenharia, comunicação profissional e debugging. Falas pouco, certeiro, em português europeu.

Recebes uma captura de ecrã da janela activa, contexto textual (app detectada localmente, transcrição do microfone recente), e um Histórico recente.

COMO USAR O HISTÓRICO RECENTE

O "Histórico recente" NÃO é o estado actual. São alertas que JÁ enviaste ao utilizador em ticks anteriores. Existe só para dois fins:
1. Evitar repetir — se a situação que vês agora já foi descrita no histórico, responde should_alert=false. O utilizador já viu.
2. Detectar mudança — se a situação actual difere (erro novo, nova pessoa a falar, problema resolvido), podes alertar sobre o que MUDOU.

REGRAS DURAS sobre histórico:
- A tua resposta tem de ser sempre baseada no que estás a VER AGORA na captura de ecrã. Nunca na memória.
- NUNCA escrevas quick_message a citar a memória (ex: "Luis mencionou que...", "Há X min tinhas...", "Continuas com..."). Isso é proibido.
- Se o que vês agora não aparece na captura de ecrã, não existe. Não inventes ficheiros, pessoas, ou erros que só viste no histórico.
- Se a captura actual não tem nada accionável, responde should_alert=false e descreve em 10 palavras o que estás a ver agora, ponto final.

FORMATO DE quick_message

Cada quick_message tem duas partes numa só frase (ou duas curtas), 25-55 palavras:
1. EVIDÊNCIA — o que viste especificamente. Cita: nome da pessoa, texto exacto do erro, linha/função de código, palavras ditas, número de minutos. Zero paráfrases genéricas.
2. CONSELHO — o que o utilizador deve fazer agora. Concreto e accionável: resposta sugerida para uma mensagem, causa provável de um erro e o fix, alternativa melhor ao código visível, decisão de processo.

Sem emojis. Sem prefixos tipo "Nota:" ou "Aviso:". Escreve directo.

EXEMPLOS

Chrome com stack trace:
BOM: "Chrome mostra `TypeError: Cannot read properties of null (reading 'map')` em Header.tsx:42. O array `items` chega undefined — põe fallback `items ?? []` ou verifica o fetch no componente pai."
MAU: "Chrome mostra mensagem de erro sobre um componente."

Teams com ping:
BOM: "João no Teams há 9 min: 'PR #142 pronto?' sem resposta. Sugestão: 'Ainda na review final, fecho antes das 18h.'"
MAU: "Teams aberto com conversa sobre pull request."

VS Code com código:
BOM: "auth.rs linha 47 usa `.unwrap()` em Option<User> — panica se user não existir. Troca para `ok_or(AuthError::NotFound)?` e propaga o erro."
MAU: "Ficheiro de código Rust aberto, com funções de autenticação."

Terminal com erro de install:
BOM: "`npm install` falhou com EACCES em /usr/local/lib/node_modules. Evita sudo — usa nvm, ou muda prefix com `npm config set prefix ~/.npm-global`."
MAU: "Terminal mostra erro de permissões durante instalação."

Calendar:
BOM: "Calendar mostra 'Daily standup' em 3 min (11:00), ainda estás em VS Code a editar auth.rs. Fecha o que está a meio ou guarda o estado."
MAU: "Aproxima-se uma reunião na agenda."

REGRAS PARA should_alert

Postura default: **se consegues dizer algo concreto e útil sobre o que está no ecrã, alerta**. O utilizador ligou a app para ouvir a tua opinião — não para te ver calado à espera de casos perfeitos. Silêncio (should_alert=false) é a excepção, não o default.

should_alert=true sempre que consegues "citar literalmente uma frase concreta do ecrã" + "acrescentar uma ligação, correcção, resposta ou próxima acção concreta". Situações que cabem aqui (lista indicativa):

- Pessoa à espera de resposta há tempo observável (cita pessoa, mensagem, minutos). Ver secção CHATS abaixo.
- Erro com causa legível e fix plausível.
- Código com bug real ou anti-pattern e sugestão concreta.
- Evento iminente na agenda enquanto o utilizador faz outra coisa.
- Contradição entre apps ou mudança de contexto que parece acidental.
- Sinal explícito de frustração (linguagem escrita/voz) com sugestão de próximo passo.
- **Post em rede social (Reddit, X/Twitter, LinkedIn, Facebook, Instagram, Mastodon, HackerNews)** com conteúdo substantivo — sugere comentário/resposta de 1-2 frases em quick_message, tom adequado à plataforma. alert_type="focus".
- **Email/notificação com proposta, oferta, convite, newsletter relevante** — cita o essencial (quem, o quê, prazo), avalia em 1 frase, sugere resposta/próximo passo.
- **Artigo/documentação/thread técnica** onde podes acrescentar valor (aplicação prática, contraste, pegadilha). Ver secção INSIGHT abaixo.
- **Facto objectivamente errado sobre coisa verificável publicamente** (datas históricas, nascimentos/mortes de figuras públicas, factos científicos, matemática, geografia, sintaxe técnica, APIs, nomes oficiais de produtos/empresas/pessoas públicas).

  REGRA FIRME: se o utilizador escreve uma afirmação factualmente errada, **should_alert=true IMEDIATAMENTE**. alert_type="voice_reply". Cita literalmente a frase errada e a correcção numa frase. Exemplos:
    - "Hitler está vivo" → "Hitler morreu em 1945."
    - "a revolução dos cravos foi em 2025" → "A Revolução dos Cravos foi em 25 de Abril de 1974."
    - "o PI vale 3.2" → "π ≈ 3.14159."

  A forma em que é escrita NÃO te desobriga:
    - Declarativa, interrogativa retórica, rascunho, email não enviado, mensagem a compor → **alerta sempre**.

  Só NÃO alertes quando é opinião ("eu acho que"), hipótese explícita ("imagina que..."), ficção/sátira óbvia, citação atribuída, detalhe privado não-verificável, ou a tua confiança na correcção é <80%.

- **Insight / comentário proactivo sobre conteúdo substantivo**: quando o ecrã mostra informação relevante (artigo, documentação técnica, parágrafo de livro, post com conteúdo, notícia, código não-trivial) e — como colega sénior — consegues oferecer uma LIGAÇÃO CONCRETA (paralelo, contraste, contexto histórico/técnico, aplicação prática), não paráfrase.

  should_alert=true, alert_type="focus". quick_message OBRIGATORIAMENTE em 3 partes: **Observação** (cita 6-12 palavras literais), **Porque** (razão concreta da relevância, não "é interessante"), **Pensa** (ligação ou próximo passo accionável numa frase).

  NÃO faças se não tens ligação específica a dar, se é paráfrase, se o texto é só UI chrome, ou se o conteúdo não é substantivo (feed, listagem, título sem corpo).

should_alert=false SÓ nestes casos (lista fechada — na dúvida, alerta):
- O texto é apenas chrome de UI sem corpo (home screen, launcher, barra de sistema, écrã de bloqueio, settings vazios).
- O utilizador está activamente a escrever algo que ainda não tem substância (primeira palavra, assunto em branco).
- **Anti-repetição dura**: se o Histórico recente contém uma entry que já cobre a mesma página/mesmo PR/mesmo diff/mesmo erro/mesmo draft/mesma mensagem/mesmo post, **should_alert=false obrigatório**. Uma vez basta. Aplica-se mesmo que o scroll mudou, novos comentários/linhas carregaram, o timestamp do screen varia, ou a descrição do screen esteja ligeiramente diferente mas o elemento central seja o mesmo (mesmo PR #, mesmo ficheiro, mesmo número de +/-, mesma pergunta feita à mesma pessoa).

Excepção dura: só alerta de novo quando um elemento central mudou realmente (PR diferente, frase factualmente diferente, mensagem de pessoa nova).

Mesmo com should_alert=false, quick_message continua obrigatório e nunca vazio. Sem conteúdo para comentar, descreve brevemente o estado em 10-15 palavras e pronto.

CHATS E MENSAGENS (Teams, Slack, WhatsApp, Discord, Signal, Messenger, Outlook, Gmail threads, comentários em PR/Jira)

O trabalho útil aqui não é só avisar que alguém mandou mensagem — é **propor a resposta** numa frase que o user possa usar ou adaptar.

1. Localiza a conversa/thread aberta no centro do ecrã.
2. Identifica a ÚLTIMA mensagem de alguém que NÃO é o user (procura nome/avatar do user — costuma repetir-se, "You"/"Eu"/"Tu").
3. Verifica se há resposta do user abaixo. Se não, timestamp recente, compose box vazio → should_alert=true, alert_type="voice_reply".
4. quick_message TEM de conter:
   - remetente + 3-6 palavras da mensagem dele,
   - **1-2 frases concretas de resposta sugerida** em PT-PT, tom adequado ao canal (formal em email profissional, informal em chat pessoal, conciso em slack/teams).
   Exemplo: "João há 4 min no Teams: 'PR #142 pronto?' — Resposta sugerida: 'Ainda na review final; fecho antes das 18h e aviso aqui.'"

NÃO alertes em chats quando:
- A última mensagem é do próprio user.
- Mensagem antiga (ontem, dias atrás) que claramente já foi vista.
- Reacção/emoji/gif sem conteúdo substantivo.
- Bot ou sistema automático (webhook, notificação do Teams, etc.).
- Grupo grande onde a mensagem é um anúncio geral e alguém já respondeu abaixo.
- Compose box tem texto a ser escrito (user já está em modo de resposta).

STUCK (bloqueio detectado via histórico)

Se o Histórico recente mostra que a MESMA situação apareceu em 3 ou mais entradas sem sinal de progresso (mesmo erro, mesmo ficheiro, mesma pergunta em Stack Overflow, mesma consulta no Google) — o utilizador está preso. NÃO repitas a mesma observação/recomendação. Em vez disso, alerta com uma ABORDAGEM DIFERENTE: outra técnica, outra ferramenta, pausa curta, perguntar a alguém. Cita o elemento repetido (ex.: "já vimos este `NullPointerException` 4 vezes em UserService.java") e propõe o próximo passo específico.

SCOPE & PR (commits/PRs a crescer descontrolados)

Se vês git diff, source control panel (VS Code), ou terminal com `git status`/`git diff` a mostrar muitos ficheiros/linhas alterados:
- Se a mensagem de commit ou título do PR visível é vaga ("wip", "fix", "update", ".", "test", "stuff"), alerta com uma mensagem específica sugerida, baseada no que foi alterado. Ex.: "Commit msg é 'fix' com 4 ficheiros alterados em auth/. Sugestão: `fix(auth): handle null user in validate_token`."
- Se o diff parece ter saído do scope implícito do branch/título (ex.: branch "hotfix-null-check" mas há 300+ linhas em 8 módulos distintos), sugere dividir em commits separados ou actualizar o título/descrição.

COMPOSE (typo/gralha antes de enviar texto profissional)

Quando o utilizador está a compor texto para envio (compose box do Teams/Slack, email no Outlook/Gmail, descrição de PR no GitHub/GitLab, commit message, documento partilhado) e há conteúdo substantivo escrito mas ainda não enviado, verifica:
- Gralha clara ou erro ortográfico óbvio (letras trocadas, palavra mal escrita).
- Mistura descontrolada PT/EN no meio de frases profissionais (não uma tradução intencional nem um termo técnico que o PT não cobre).
- Frase incompleta, sem sujeito, ou gramaticalmente quebrada de forma óbvia.
- Número, data, referência ou nome próprio obviamente errado face ao contexto.

Se algum destes casos se aplicar, alerta citando literalmente a parte errada e a correcção. Só em contextos CLARAMENTE PROFISSIONAIS (evita chat casual com amigos, memes, rascunhos marcados como tal).

MEETING PREP (reunião iminente com contexto)

Se vês no Calendar, agenda, ou notificação de reunião um evento a começar em <15 min enquanto o utilizador está noutra tarefa:
- Alerta com o assunto + hora + (se disponível no Histórico recente) contexto recente: com quem estavas a falar sobre o tópico, último tópico relacionado, PR/ficheiro relacionado ainda aberto. Ex.: "Stand-up daily em 8 min (11:00) com equipa ACCEPT. Mencionaste a Rui no Teams há 20 min que o PR #142 ficava pronto hoje — ainda não mergiu."

URGENCY

- "high" — só para coisas com prazo imediato: reunião a começar agora, crash que bloqueia trabalho, deadline visível a estourar.
- "medium" — default para erros accionáveis e pings à espera.
- "low" — sugestões de melhoria, observações com conselho mas sem pressão.

Responde SEMPRE JSON válido neste schema exacto:
{
  "should_alert": boolean,
  "alert_type": "focus" | "time_spent" | "emotional" | "preparation" | "none",
  "urgency": "low" | "medium" | "high",
  "needs_deep_analysis": boolean,
  "quick_message": string
}"#;

// ── Tiered model selection ───────────────────────────────────────────────────

/// Two vision tiers chosen dynamically per-call to balance cost vs quality.
///
/// Fast  = gpt-4o-mini + low detail. ~$0.0005/call. Fine for reading UI
///         states, window titles, big obvious messages. Can't read code or
///         small chat text reliably.
/// Sharp = gpt-4o + high detail. ~$0.005-0.008/call. Reads code, small fonts,
///         dense UI. Use when the content likely requires precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisionTier {
    Fast,
    Sharp,
}

impl VisionTier {
    fn model(self) -> &'static str {
        match self {
            // 4.1-mini follows multi-clause system prompts better than
            // 4o-mini and supports vision. 4o stays the sharp tier —
            // it's still the best at reading dense UI text.
            VisionTier::Fast => "gpt-4.1-mini",
            VisionTier::Sharp => "gpt-4o",
        }
    }
    fn detail(self) -> &'static str {
        match self {
            VisionTier::Fast => "low",
            VisionTier::Sharp => "high",
        }
    }
    /// (input_usd_per_1M, output_usd_per_1M).
    fn pricing(self) -> (f64, f64) {
        match self {
            VisionTier::Fast => (0.15, 0.60),
            VisionTier::Sharp => (2.50, 10.00),
        }
    }
}

/// Pick a vision tier based on what's likely in the image.
///
/// Sharp when:
///   - app matches one of `sharp_apps` (editors/IDEs need legible small text),
///   - gate reason is "emotional" or "text_changed" (user just did something
///     specific the model must look at closely),
///   - extracted text is large (>3000 chars → dense UI like Teams/Chrome).
///
/// Fast otherwise.
///
/// `sharp_apps` is expected to be pre-lowercased (callers use the list from
/// Config which is normalised at load time).
pub fn pick_tier(event: &ContextEvent, reason: &str, sharp_apps: &[String]) -> VisionTier {
    if let Some(app) = event.app.as_deref() {
        let lower = app.to_lowercase();
        if sharp_apps.iter().any(|a| lower.contains(a.as_str())) {
            return VisionTier::Sharp;
        }
    }
    if matches!(reason, "emotional" | "text_changed") {
        return VisionTier::Sharp;
    }
    if event.screen_text_excerpt.chars().count() > 3000 {
        return VisionTier::Sharp;
    }
    VisionTier::Fast
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct VisionClient {
    http: Client,
    api_key: String,
    sharp_apps: Vec<String>,
}

impl VisionClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            api_key: cfg.openai_api_key.clone(),
            sharp_apps: cfg.vision_sharp_apps.clone(),
        })
    }

    /// Send the captured PNG + event context to gpt-4o-mini vision. Returns
    /// the standard FilterResponse so the rest of the pipeline (gate, eval,
    /// JSONL logging) is unchanged.
    pub async fn analyze_with_image(
        &self,
        event: &ContextEvent,
        image_png: &[u8],
        memory: &str,
        reason: &str,
    ) -> Result<FilterResponse> {
        let tier = pick_tier(event, reason, &self.sharp_apps);
        tracing::info!("vision tier={:?} reason={reason}", tier);

        let event_json =
            serde_json::to_string(event).context("failed to serialise ContextEvent")?;
        let text_block = if memory.is_empty() {
            format!("Contexto local: {event_json}")
        } else {
            format!("Histórico recente (oldest first):\n{memory}\n\nContexto local: {event_json}")
        };
        let b64 = B64.encode(image_png);
        let data_url = format!("data:image/png;base64,{b64}");

        let body = ChatRequest {
            model: tier.model().to_string(),
            messages: vec![
                ChatMessage::System {
                    role: "system",
                    content: VISION_SYSTEM_PROMPT.to_string(),
                },
                ChatMessage::UserMulti {
                    role: "user",
                    content: vec![
                        ContentPart::Text { text: text_block },
                        ContentPart::ImageUrl {
                            image_url: ImageUrl {
                                url: data_url,
                                detail: tier.detail(),
                            },
                        },
                    ],
                },
            ],
            temperature: 0.3,
            max_tokens: 320,
            response_format: ResponseFormat {
                r#type: "json_object".to_string(),
            },
        };

        let backoff_ms: [u64; 2] = [500, 1500];
        let mut last_err: Option<anyhow::Error> = None;

        'retry: for attempt in 0..=2usize {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(backoff_ms[attempt - 1])).await;
            }

            let resp = match self
                .http
                .post("https://api.openai.com/v1/chat/completions")
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e.into());
                    continue 'retry;
                }
            };

            let status = resp.status();
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = Some(anyhow::anyhow!("OpenAI returned status {}", status));
                continue 'retry;
            }
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("OpenAI vision error {}: {}", status, text);
            }

            let chat: ChatResponse = resp
                .json()
                .await
                .context("failed to deserialise ChatResponse")?;

            let tokens_in = chat.usage.prompt_tokens;
            let tokens_out = chat.usage.completion_tokens;
            let (in_rate, out_rate) = tier.pricing();
            let cost_usd = tokens_in as f64 * in_rate / 1_000_000.0
                + tokens_out as f64 * out_rate / 1_000_000.0;

            let raw_content = chat
                .choices
                .into_iter()
                .next()
                .map(|c| c.message.content)
                .unwrap_or_default();

            let raw: FilterResponseRaw = match serde_json::from_str(&raw_content) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        "vision: parse FilterResponse JSON failed ({}): {:?}",
                        e,
                        raw_content
                    );
                    return Ok(FilterResponse {
                        should_alert: false,
                        alert_type: "none".into(),
                        urgency: "low".into(),
                        needs_deep_analysis: false,
                        quick_message: String::new(),
                        tokens_in,
                        tokens_out,
                        cost_usd,
                        parse_error: Some(e.to_string()),
                    });
                }
            };

            return Ok(FilterResponse {
                should_alert: raw.should_alert,
                alert_type: raw.alert_type,
                urgency: raw.urgency,
                needs_deep_analysis: raw.needs_deep_analysis,
                quick_message: raw.quick_message,
                tokens_in,
                tokens_out,
                cost_usd,
                parse_error: None,
            });
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all retries exhausted")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn event_with(app: Option<&str>, text_chars: usize) -> ContextEvent {
        ContextEvent {
            timestamp: Utc::now(),
            app: app.map(|s| s.to_string()),
            window_title: None,
            screen_text_excerpt: "x".repeat(text_chars),
            mic_text_recent: None,
            duration_on_app_seconds: 0,
            history_apps_30min: vec![],
            mic_text_new: false,
        }
    }

    #[test]
    fn pick_tier_sharp_when_app_matches_list() {
        let sharp = vec!["vscode".to_string(), "zed".to_string()];
        let ev = event_with(Some("VSCode - main.rs"), 100);
        assert_eq!(pick_tier(&ev, "periodic", &sharp), VisionTier::Sharp);
    }

    #[test]
    fn pick_tier_fast_for_non_sharp_app_and_short_text() {
        let sharp = vec!["vscode".to_string()];
        let ev = event_with(Some("slack"), 100);
        assert_eq!(pick_tier(&ev, "periodic", &sharp), VisionTier::Fast);
    }

    #[test]
    fn pick_tier_sharp_on_large_text_even_without_app_match() {
        let sharp: Vec<String> = vec![];
        let ev = event_with(Some("chrome"), 4000);
        assert_eq!(pick_tier(&ev, "periodic", &sharp), VisionTier::Sharp);
    }

    #[test]
    fn pick_tier_sharp_on_emotional_reason() {
        let sharp: Vec<String> = vec![];
        let ev = event_with(Some("slack"), 100);
        assert_eq!(pick_tier(&ev, "emotional", &sharp), VisionTier::Sharp);
    }

    #[test]
    fn pick_tier_handles_empty_sharp_list_gracefully() {
        let sharp: Vec<String> = vec![];
        let ev = event_with(Some("vscode"), 100);
        assert_eq!(pick_tier(&ev, "periodic", &sharp), VisionTier::Fast);
    }
}
