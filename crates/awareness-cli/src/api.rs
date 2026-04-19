use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use crate::aggregator::ContextEvent;
use crate::config::Config;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterResponse {
    pub should_alert: bool,
    pub alert_type: String,       // "focus"|"time_spent"|"emotional"|"preparation"|"none"
    pub urgency: String,          // "low"|"medium"|"high"
    pub needs_deep_analysis: bool,
    pub quick_message: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
    /// Set when the model's response could not be parsed as the expected JSON
    /// schema. Tokens were still spent — caller should deduct `cost_usd` but
    /// must NOT treat other fields as meaningful signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

// ── Internal request / response structs ──────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    r#type: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Choice {
    message: MessageContent,
}

#[derive(Deserialize)]
struct MessageContent {
    content: String,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[derive(Deserialize)]
struct FilterResponseRaw {
    should_alert: bool,
    alert_type: String,
    urgency: String,
    needs_deep_analysis: bool,
    quick_message: String,
}

// ── System prompt ─────────────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = r#"És um colega sénior que acompanha o ecrã do utilizador. Não és observador passivo — és alguém que ajuda, antecipa, e sugere acções concretas. Tens experiência em engenharia, comunicação profissional e debugging. Falas pouco, certeiro, em português europeu.

Recebes TEXTO extraído da janela activa (árvore de acessibilidade ou OCR), app detectada, transcrição recente do microfone, e um Histórico recente. Não recebes imagem — trabalha só com o texto que tens.

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

REGRAS PARA should_alert

should_alert=true apenas quando existe UMA DAS SEGUINTES e tens detalhe específico para citar E conselho concreto para dar:
- Pessoa à espera de resposta há tempo (cita pessoa, mensagem, minutos). Ver secção CHATS abaixo.
- Erro com causa legível no texto e fix plausível.
- Código com bug real ou anti-pattern visível e sugestão concreta.
- Evento iminente na agenda enquanto o utilizador faz outra coisa.
- Contradição entre apps ou mudança de contexto acidental.
- Sinal explícito de frustração (texto ou voz) com sugestão de próximo passo.
- **Facto errado em texto que o utilizador está a escrever** (documento, email, mensagem, chat, wiki) sobre algo verificável publicamente (datas históricas, factos científicos, matemática, sintaxe técnica, APIs, nomes oficiais). APENAS se tens ≥90% de confiança. Cita literalmente o que escreveu e indica o que é correcto.
  - NÃO alertes sobre: opiniões, juízos, frases hipotéticas, especulação, ficção, sarcasmo, citações atribuídas a outros, nomes próprios obscuros, detalhes privados, rascunhos.
  - Se ambíguo, NÃO alertes.

should_alert=false nos restantes casos, incluindo:
- Utilizador está a trabalhar sem sinal de bloqueio.
- Não tens detalhe específico para nomear.
- Não tens conselho concreto para dar.
- A MESMA situação já aparece no Histórico recente — não repitas. Se continua visível, assume que o utilizador viu. Excepção: se a situação mudou (erro diferente, nova mensagem), alerta descrevendo o que mudou.

quick_message continua obrigatório mesmo com should_alert=false. Sem conteúdo para comentar, descreve brevemente o estado em 10-15 palavras.

CHATS (Teams, Slack, WhatsApp, Discord, Signal, Messenger, Outlook)

Quando o texto extraído é de uma app de chat ou email:
1. No texto, procura linhas que correspondam a mensagens recentes (costumam ter padrão `Nome [tempo/timestamp]: texto`, ou avatar + nome + mensagem).
2. Identifica o nome do user (repete-se muito, normalmente próximo de "Sent from", "You", "Eu", ou como autor recorrente).
3. Encontra a ÚLTIMA mensagem atribuída a alguém que não é o user, com timestamp recente.
4. Se não há mensagem do user depois → alerta citando remetente, texto curto, tempo, e resposta sugerida concreta.

NÃO alertes em chats quando:
- Última mensagem é do próprio user.
- Mensagem antiga (ontem/dias atrás).
- Reacção/emoji/bot/sistema automático.
- Grupo onde alguém já respondeu.
- Texto insuficiente para distinguir quem escreveu o quê.

STUCK (bloqueio via histórico)

Se o Histórico recente mostra a MESMA situação em 3+ entradas sem progresso (mesmo erro, mesmo ficheiro, mesma consulta), alerta com uma ABORDAGEM DIFERENTE, não repitas a mesma observação. Cita o elemento repetido e propõe o próximo passo concreto.

SCOPE & PR (commits/PRs descontrolados)

Se vês output de `git diff`/`git status` com muitos ficheiros/linhas alterados E uma mensagem de commit ou título de PR visível que é vaga ("wip", "fix", "update", "."), alerta com mensagem específica sugerida baseada no texto do diff. Se o diff é muito maior do que o título sugere, recomenda dividir.

COMPOSE (gralha antes de enviar texto profissional)

Quando detectas que o utilizador está a compor texto para envio (email, PR description, commit message, compose box de chat) e há conteúdo substantivo:
- gralha óbvia, erro ortográfico, mistura PT/EN descontrolada no meio de texto profissional, frase gramaticalmente quebrada, número/data errado face ao contexto →
alerta citando a parte errada e a correcção. Só em contextos profissionais claros (não chat casual).

MEETING PREP (reunião iminente)

Se o texto mostra notificação/evento de calendar a começar em <15 min, alerta com assunto + hora + contexto relevante do Histórico recente (com quem estavas a falar sobre o tema, ficheiro/PR relacionado ainda aberto).

URGENCY

- "high" — prazo imediato (reunião a começar agora, crash bloqueante, deadline a estourar).
- "medium" — default para erros accionáveis e pings à espera.
- "low" — sugestões de melhoria, observações com conselho sem pressão.

Responde SEMPRE JSON válido neste schema exacto:
{
  "should_alert": boolean,
  "alert_type": "focus" | "time_spent" | "emotional" | "preparation" | "none",
  "urgency": "low" | "medium" | "high",
  "needs_deep_analysis": boolean,
  "quick_message": string
}"#;

// ── Client ────────────────────────────────────────────────────────────────────

pub struct OpenAiClient {
    http: Client,
    api_key: String,
}

impl OpenAiClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            api_key: cfg.openai_api_key.clone(),
        })
    }

    pub async fn filter_call(&self, event: &ContextEvent, memory: &str) -> Result<FilterResponse> {
        let event_json = serde_json::to_string(event)
            .context("failed to serialise ContextEvent")?;
        let user_content = if memory.is_empty() {
            event_json
        } else {
            format!("Histórico recente (oldest first):\n{memory}\n\nContexto actual:\n{event_json}")
        };

        let body = ChatRequest {
            model: "gpt-4o-mini".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_content,
                },
            ],
            temperature: 0.3,
            max_tokens: 280,
            response_format: ResponseFormat {
                r#type: "json_object".to_string(),
            },
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

            // Retryable errors
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = Some(anyhow::anyhow!("OpenAI returned status {}", status));
                continue 'retry;
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("OpenAI error {}: {}", status, text);
            }

            let chat: ChatResponse = resp
                .json()
                .await
                .context("failed to deserialise ChatResponse")?;

            let tokens_in = chat.usage.prompt_tokens;
            let tokens_out = chat.usage.completion_tokens;
            let cost_usd =
                tokens_in as f64 * 0.15 / 1_000_000.0 + tokens_out as f64 * 0.60 / 1_000_000.0;

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

    fn sample(parse_error: Option<String>) -> FilterResponse {
        FilterResponse {
            should_alert: false,
            alert_type: "none".into(),
            urgency: "low".into(),
            needs_deep_analysis: false,
            quick_message: String::new(),
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: 0.000018,
            parse_error,
        }
    }

    #[test]
    fn parse_error_absent_when_none() {
        let r = sample(None);
        let s = serde_json::to_string(&r).unwrap();
        // skip_serializing_if = "Option::is_none" must drop the field entirely.
        assert!(!s.contains("parse_error"), "serialized JSON should omit parse_error: {s}");
    }

    #[test]
    fn parse_error_present_when_some() {
        let r = sample(Some("schema mismatch".into()));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("parse_error"), "JSON must contain parse_error field: {s}");
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
