use crate::config::Config;
use crate::types::{ContextEvent, FilterResponse};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

EXCEPÇÃO DE TAMANHO: em modo SCROLL/FEED SOCIAL (ver secção dedicada mais abaixo) o quick_message é prosa corrida 50-90 palavras, não 25-55. Todas as outras regras de forma (sem rótulos, sem emojis, directo, português europeu) continuam a aplicar-se.

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

Email com lista de ofertas de trabalho (Remote Rocketship, Otta, Gun.io, LinkedIn newsletter):
BOM: "Remote Rocketship: 'Senior Rust Engineer @ Figma — remoto EU — $180-220k'. Combina com o teu stack (Rust + distributed). Pensa: candidata hoje, ligação com o recruiter via LinkedIn nas próximas 48h."
MAU: "Email com lista de empregos remotos."

Post no Instagram com dev-tip:
BOM: "Abhijit no IG: 'adiciona ao .md — Codex will review your output once you are done'. 100x é marketing, mas cross-model review é prática real (tens subagent code-reviewer). Pensa: testa num commit pequeno antes de meter no fluxo."
MAU: "Post do Instagram sobre uso do Claude Code."

LinkedIn com anúncio de nova posição pública:
BOM: "João Silva anunciou Head of Platform @ Empresa X. Ele era CTO da antiga start-up do Y. Pensa: congratular hoje + pedir 30 min próxima semana — é contacto directo para roles remotos."
MAU: "Um contacto mudou de emprego no LinkedIn."

REGRAS PARA should_alert

Postura default: **se consegues dizer algo concreto e útil sobre o que está no ecrã, alerta**. O utilizador ligou a app para ouvir a tua opinião — não para te ver calado à espera de casos perfeitos. Silêncio (should_alert=false) é a excepção, não o default, e aplica-se só nos casos listados mais abaixo.

should_alert=true sempre que consegues cumprir: "citar literalmente uma frase concreta do ecrã" + "acrescentar uma ligação, correcção, resposta ou próxima-acção concreta que o utilizador não teria de outra forma". Exemplos de situações que cabem aqui (lista indicativa, não exaustiva):

- Pessoa à espera de resposta há tempo (cita pessoa, mensagem, minutos). Ver secção CHATS abaixo.
- Erro com causa legível no texto e fix plausível.
- Código com bug real ou anti-pattern visível e sugestão concreta.
- Evento iminente na agenda enquanto o utilizador faz outra coisa.
- Contradição entre apps ou mudança de contexto acidental.
- Sinal explícito de frustração (texto ou voz) com sugestão de próximo passo. Usa alert_type="emotional".
- **Post em rede social (Reddit, X/Twitter, LinkedIn, Facebook, Instagram, Mastodon, HackerNews)** com conteúdo substantivo onde consegues acrescentar valor: contra-argumento, contexto técnico, experiência pessoal análoga, link mental para outra ideia. Alerta com alert_type="focus" e sugere um comentário/resposta de 1-2 frases em quick_message. NOTA: se a app activa é um dos pacotes de scroll/feed social listados na secção "CONTEÚDO DE SCROLL / FEED SOCIAL" mais abaixo (IG/TikTok/Shorts/FB/X/Reddit/Snapchat/Pinterest/LinkedIn), usa o FORMATO SCROLL dessa secção em vez do formato 3-partes do modo Insight.
- **Email ou notificação com proposta, oferta, convite** (entrevista, oferta de trabalho, proposta de projecto, convite para evento, newsletter com notícia relevante à carreira/interesses do utilizador). Alerta citando o essencial (quem, o quê, prazo), avalia em 1 frase (se é interessante, riscos, próximo passo natural) e sugere uma resposta concreta quando aplicável.
- **Artigo / documentação / thread técnica** em que o conteúdo cruza com algo que valha a pena notar — aplicação prática, contraste com prática comum, truque não-óbvio, pegadilha. Ver secção INSIGHT abaixo.
- **Pergunta ou comando falado**: se mic_text_recent contém uma pergunta directa ("o que é X?", "qual a diferença entre A e B?", "como faço Y?") ou um comando ("lembra-me de…", "resume isto", "explica-me…"), RESPONDE em quick_message com alert_type="voice_reply". Cita a pergunta em 3-6 palavras e dá uma resposta concreta de 1-2 frases. Se não sabes responder com certeza, diz o que é preciso para responder em vez de inventar.
- **Sinal emocional/stress só por voz**: se o tom ou as palavras em mic_text_recent indicam frustração, confusão ou cansaço (mesmo sem keywords explícitas), alert_type="emotional". Cita a frase curta e propõe 1 passo concreto (pausa, próximo debug step, reformular abordagem).
- **Facto objectivamente errado sobre coisa verificável publicamente** (datas históricas, nascimentos/mortes de figuras públicas, factos científicos, matemática, geografia, sintaxe técnica, APIs, nomes oficiais de produtos/empresas/pessoas públicas).

  REGRA FIRME: se o utilizador escreve uma afirmação factualmente errada, **should_alert=true IMEDIATAMENTE**. alert_type="voice_reply". Cita literalmente o que escreveu e a correcção numa frase. Exemplos concretos:
    - Utilizador escreve "Hitler está vivo" → "Hitler morreu em 1945."
    - Utilizador escreve "a revolução dos cravos foi em 2025" → "A Revolução dos Cravos foi em 25 de Abril de 1974."
    - Utilizador escreve "o PI vale 3.2" → "π ≈ 3.14159."

  A forma em que é escrita NÃO te desobriga:
    - Declarativa ("Hitler está vivo") → alerta.
    - Interrogativa retórica ("Hitler está vivo?", "foi em 2025 certo?") → alerta.
    - Rascunho / email a compor / mensagem não enviada → **alerta, é exactamente para isso que ele te quer**.
    - Contido num parágrafo mais longo → alerta mesmo assim, cita a frase errada.

  Só NÃO alertes quando:
    - É opinião declarada como tal ("eu acho que X", "na minha opinião Y").
    - É hipótese explícita ("imagina que...", "e se...").
    - É ficção, sátira ou sarcasmo óbvio.
    - É citação atribuída a outros ("como disse o X, ...").
    - É detalhe privado não-verificável (endereços, nomes internos da empresa, agenda pessoal).
    - A tua confiança na correcção é <80%.

- **Insight / comentário proactivo sobre conteúdo substantivo**: quando o ecrã mostra informação relevante (artigo, documentação técnica, parágrafo de livro, post em rede social com conteúdo, tese, notícia, código não-trivial) e — como colega sénior — consegues oferecer uma LIGAÇÃO CONCRETA que valha a pena partilhar. Não é paráfrase; é conhecimento adicional.

  should_alert=true, alert_type="focus". quick_message OBRIGATORIAMENTE em 3 partes:
    1. **Observação**: cita literalmente a frase/ideia em 6-12 palavras.
    2. **Porque**: razão concreta da relevância — paralelo com outra ideia, contraste, contexto histórico/técnico, aplicação prática. NÃO "é interessante"; SIM "lembra o X que viste", "contraria Y", "aplica-se em Z".
    3. **Pensa**: sugestão accionável em 1 frase — uma ligação para explorar, um próximo passo, uma consequência.

  Exemplo:
    - "Observação: 'async/await in Rust uses state machines compiled by the compiler.' Porque: explica por que Future precisa de Pin quando o stack frame não pode mover. Pensa: aplicar o mesmo raciocínio à Vec<Arc<Mutex<…>>> que rejeitaste há pouco — talvez Box::pin resolva."

  NÃO faças se:
    - Não tens ligação específica, só paráfrase.
    - A ligação é trivialmente óbvia ("este artigo fala de X" — não, a ligação é o que tu acrescentas).
    - O texto é apenas chrome de UI (menus, toolbars, barras de status).
    - O conteúdo não é substantivo (feed, listagem, título sem corpo).

should_alert=false SÓ nestes casos (lista fechada — na dúvida, alerta):
- O texto é apenas chrome de UI sem corpo (home screen, launcher, barra de sistema, écrã de bloqueio, settings vazios).
- O utilizador está activamente a escrever algo que ainda não tem substância (primeira palavra, assunto em branco).
- **Anti-repetição dura**: se o Histórico recente contém uma entry que já cobre a mesma página/mesmo PR/mesmo diff/mesmo erro/mesmo draft/mesma mensagem/mesmo post, **should_alert=false obrigatório**. Uma vez basta — o utilizador já viu. Isto aplica-se mesmo que:
    - o scroll mudou,
    - novos comentários/linhas carregaram,
    - o timestamp do screen varie,
    - a descrição do screen esteja ligeiramente diferente mas o elemento central seja o mesmo (mesmo PR #, mesmo ficheiro, mesmo número de linhas +/-, mesma pergunta feita à mesma pessoa).
  Só voltas a alertar quando um elemento central mudou realmente (PR diferente, frase factualmente diferente, mensagem de pessoa nova a chegar).

quick_message continua obrigatório mesmo com should_alert=false. Sem conteúdo para comentar, descreve brevemente o estado em 10-15 palavras.

CHATS E MENSAGENS (Teams, Slack, WhatsApp, Discord, Signal, Messenger, Outlook, Gmail threads, comentários em PR/Jira)

Quando o utilizador está a LER uma mensagem/email/comentário que alguém lhe enviou (e ainda não respondeu), o trabalho útil não é só alertar — é **propor a resposta**. Tratamento:

1. Extrai do texto as linhas de mensagens recentes (padrão `Nome [tempo]: texto` ou avatar + nome + corpo).
2. Identifica o dono do dispositivo (autor que se repete mais / próximo de "You", "Eu", "Sent from", "Enviado de").
3. Encontra a ÚLTIMA mensagem que NÃO é dele, com timestamp recente.
4. Se ainda não respondeu → should_alert=true, alert_type="voice_reply".
5. quick_message TEM de incluir:
   - remetente + 3-6 palavras da mensagem dele,
   - **1-2 frases concretas de resposta sugerida**, em português europeu, tom adequado ao contexto (formal em email profissional, informal em chat pessoal, conciso em slack/teams).
   Exemplo: `João no Teams há 4 min: 'PR #142 pronto?' — Resposta sugerida: "Ainda na review final; fecho antes das 18h e aviso aqui."`

NÃO alertes em chats quando:
- Última mensagem é do próprio user (já respondeu).
- Mensagem antiga (ontem/dias atrás, sem novo ping).
- Reacção/emoji/bot/sistema automático.
- Grupo onde alguém já respondeu ao interlocutor.
- Texto insuficiente para saber quem escreveu o quê.
- Já alertaste sobre esta exacta mensagem recentemente (ver anti-repetição acima).

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

FORMATO SCROLL (quick_message, 50-90 palavras, prosa corrida, SEM rótulos "Observação:"/"Porque:"/"Pensa:"/"Verificação:", SEM bullets, SEM linhas separadas):

1. Começa com um RESUMO do post/reel em 1-2 frases concretas: o que mostra, quem aparece, acção central. Cita frase literal curta se for essencial (título, legenda, quote).
2. Continua com INSIGHT ou CONTEXTO do tema: origem do trend, dado histórico ou técnico, quem são os intervenientes, porque o tema tem tracção, aplicação prática. Deve acrescentar algo que o reel sozinho não dá.
3. SE a afirmação central é objectivamente verificável (facto histórico, dado técnico, número, sintaxe, atribuição, afirmação científica) E a tua confiança é ≥80% E o veredicto é claro ("verdadeiro" ou "falso"), incorpora a verificação na prosa ("na realidade X", "confirma-se que Y", "é incorrecto: a data foi Z"). SE a claim é opinião, humor, ficção, subjectiva ou a tua confiança é <80%, OMITE a verificação. Não inventes verificações nem respondas com "depende".
4. SE cruzar com o bio do utilizador ou um interesse explícito, fecha com uma ligação concreta. Se não houver ligação natural, termina sem forçar.

Tom: português europeu directo, frases curtas ligadas entre si, sem emojis, sem prefixos tipo "Nota:", sem rótulos.

EXEMPLO (caso real Jim Beam + Coca-Cola):

MAU (formato 3-partes com labels, NÃO uses): "Texto: 'The post shares a viral video of a father at a table with his son, pouring Coca-Cola into glasses before secretly adding a generous amount of Jim Beam whiskey.' Porque: mistura humor com exposição potencialmente controversa sobre hábitos de álcool perto de crianças. Pensa: avaliar impacto UX e ética antes de integrar conteúdos similares em apps focadas em famílias ou crianças."

BOM (FORMATO SCROLL, ~70 palavras, prosa corrida): "Reel viral de um pai a servir Coca-Cola ao filho e, disfarçadamente, juntar Jim Beam — formato dad-prank que circula em TikTok/IG desde 2022 e já gerou várias ondas de 'bourbon and coke' memes. Jim Beam é bourbon americano (cerca de 40% ABV); o gesto é encenado para o vídeo, não é prática comum registada. O ângulo que faz espalhar é o choque fake, não receita nenhuma."

Anti-repetição: as regras existentes mantêm-se — se já alertaste sobre este mesmo post no Histórico recente, should_alert=false.

Quando NÃO alertar em scroll: feed-chrome sem post aberto, listagem de thumbnails, ecrã de loja/settings da app, DMs (essas caem em CHATS mais acima), post que é só UI (sem caption nem texto OCR substantivo).

alert_type continua "focus" nestes casos (para manter o routing do cliente). A urgência é quase sempre "low" — scroll não é urgente.

URGENCY

- "high" — prazo imediato (reunião a começar agora, crash bloqueante, deadline a estourar).
- "medium" — default para erros accionáveis e pings à espera.
- "low" — sugestões de melhoria, observações com conselho sem pressão.

Responde SEMPRE JSON válido neste schema exacto:
{
  "should_alert": boolean,
  "alert_type": "focus" | "time_spent" | "emotional" | "preparation" | "voice_reply" | "none",
  "urgency": "low" | "medium" | "high",
  "needs_deep_analysis": boolean,
  "quick_message": string
}"#;

// ── Client ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct OpenAiClient {
    http: Client,
    api_key: String,
}

impl OpenAiClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        Self::with_api_key(cfg.openai_api_key.clone())
    }

    /// Build an `OpenAiClient` directly from an API key, skipping the full
    /// `Config` struct. Used by the Android frontend, which assembles
    /// context on the Kotlin side and doesn't need CLI-specific config fields.
    pub fn with_api_key(api_key: String) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { http, api_key })
    }

    pub async fn filter_call(
        &self,
        event: &ContextEvent,
        memory: &str,
        user_profile: &str,
        matched_interests: &[String],
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
        let user_content = if memory.is_empty() {
            format!("{interests_line}{event_json}")
        } else {
            format!(
                "Histórico recente (oldest first):\n{memory}\n\n{interests_line}Contexto actual:\n{event_json}",
            )
        };

        // Prepend any accumulated user profile (bio + interests +
        // anti-interests + top apps) to the system content. Keeps the
        // original instruction block intact while giving the model a
        // concrete picture of who the user is and what kind of alerts
        // they've opted into.
        let system_content = if user_profile.trim().is_empty() {
            SYSTEM_PROMPT.to_string()
        } else {
            format!(
                "PERFIL DO UTILIZADOR (prioriza isto ao decidir o que é relevante):\n{}\n\n---\n\n{}",
                user_profile.trim(),
                SYSTEM_PROMPT,
            )
        };

        let body = ChatRequest {
            // gpt-4o-mini was silently dropping alerts even after
            // recognising actionable content ("visualizar lista de
            // empregos remotos"). gpt-4.1-mini follows longer system
            // prompts with competing clauses much better — the
            // "postura default: alerta" instruction actually takes.
            model: "gpt-4.1-mini".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_content,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_content,
                },
            ],
            temperature: 0.3,
            // 500 so reply-suggestion messages for emails/chats have
            // room for both citation + reply draft. 280 was clipping
            // the useful part.
            max_tokens: 500,
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
                        matched_interests: matched_interests.to_vec(),
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
                matched_interests: matched_interests.to_vec(),
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
            matched_interests: Vec::new(),
        }
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
