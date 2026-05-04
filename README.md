# WebTransport 1C Addin (Rust)

Внешняя компонента для 1С, объединяющая WebSocket‑клиент и HTTP/SSE сервер с обменом событиями с 1С.

Проект основан на оригинальном репозитории и шаблоне внешней компоненты на Rust:
- Первоисточник: https://github.com/dlyubanevich/websocket1c
- Шаблон компоненты (Rust): https://github.com/medigor/addin1c

## Состав и имена классов

Компонента экспортирует 3 класса (имена для `Новый("AddIn.*")`):
- `ws` — WebSocket‑клиент. См. [docs/ws.md](docs/ws.md).
- `http` — HTTP/SSE сервер с событиями в 1С. См. [docs/http.md](docs/http.md).
- `mcp` — MCP Streamable HTTP сервер (JSON‑only). См. [docs/mcp.md](docs/mcp.md).
- `session` — **(добавлен в форке)** WS‑клиент к `v8-client-session-manager` с auto‑reconnect, correlation‑id, host_id/pid/capabilities и системными методами `addin.spawn` / `addin.kill`.

## Доработки форка SteelMorgan относительно upstream

Этот форк добавляет класс `session` (WS-клиент к `v8-client-session-manager`), системные методы `addin.spawn` / `addin.kill`, auto-reconnect, tracing-инфраструктуру и расширяет `SessionParams` полями `host_id` / `pid` / `capabilities`. Версия компоненты — **0.7.0** (upstream baseline 0.6.4, +12 коммитов).

Полное описание изменений, распределение ответственности между Rust- и BSL-сторонами (установка WS-соединения, пинги, `session.register`) — см. **[FORK_CHANGES.md](FORK_CHANGES.md)**.
