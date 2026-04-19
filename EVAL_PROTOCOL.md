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
| Custo médio diário | < $0.30 |
| CPU médio | < 20% |
| RAM pico | < 1GB |
| Latência tick→alert (p95) | < 5s |

Falha em **qualquer uma** → iterar ou abandonar. Passa em **todas** → continuar para MVP propriamente dito.

## Duração do teste
5 dias úteis consecutivos, mínimo 6h/dia.

## Output final
`data/phase_poc/report.md` gerado por `analyze_runs.py` com:
- Métricas agregadas
- Top 10 alertas por rating
- Breakdown por app
- Decisão go/no-go.
