# Eval Protocol

Metric definitions fixas ANTES de começar qualquer dogfooding.

## Rating scale
- `useful` (u): alerta que te fez mudar comportamento ou reparar em algo que não tinhas reparado.
- `not_useful` (n): alerta correcto mas irrelevante — não fez nada por ti.
- `annoying` (a): alerta errado ou timing mau.

## Regra de rating
- Rating é dado na **primeira ocorrência visível** do alerta.
- Não se re-rateia.
- Se não respondeste a um alerta em <5min → fica `null` e conta como `not_useful` na análise.

## Critérios de sucesso (POC go/no-go)

| Métrica | Threshold |
|---|---|
| `useful_rate` (useful / (useful + not_useful + annoying)) | ≥ 40% |
| Alerts/hora em horário de trabalho | 2-8 |
| CPU médio (laptop) | < 20% |
| RAM pico (laptop) | < 1GB |
| Latência tick→alert (p95) | < 15s warm / < 20s cold |

Notas:
- Custo deixou de ser métrica (LLM local via Ollama no OMEN, $0/dia).
- Medições reais com qwen3:8b Q4 na RTX 2060 após (a) trim do system
  prompt para ~970 tokens, (b) switch para `/api/chat` nativo,
  (c) `num_ctx=6144` explícito, (d) `keep_alive=30m`: warm steady
  9-12s, cold ~12s, p95 estimada ~14s. Modelo fica 100% GPU
  (`ollama ps` mostra `5.9 GB 100% GPU 6144`).
- O shim `/v1/chat/completions` IGNORA `options.num_ctx`. Se voltares
  a usá-lo, Ollama defaulta 8192 → ~14% layers spill para CPU na 2060
  → latência sobe para 16-20s warm. Mantém-te no `/api/chat` nativo.

Falha em **qualquer uma** → iterar ou abandonar. Passa em **todas** → continuar para MVP propriamente dito.

## Duração do teste
5 dias úteis consecutivos, mínimo 6h/dia.

## Output final
`data/phase_poc/report.md` gerado por `analyze_runs.py` com:
- Métricas agregadas
- Top 10 alertas por rating
- Breakdown por app
- Decisão go/no-go.
