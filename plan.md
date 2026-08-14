# Opt-Drive — Plano & Roadmap

Documento vivo do que já está pronto e dos próximos passos, **passo a passo**,
para que qualquer pessoa (ou agente) possa executar cada tarefa.

> Convenção de status: ✅ feito · 🔵 em andamento · ⬜ a fazer

---

## 0. Estado atual (Fase 1 — MVP) ✅

Arquitetura entregue e validada na máquina-alvo (Windows):

- **`opt-drive-core`** — lógica de domínio (config, drives, index/SQLite, usage,
  policy, ops, providers, **catálogo de cleanup**).
- **`opt-drive-daemon`** — serviço REST + WebSocket + scheduler.
- **`opt-drive-cli`** — CLI `opt-drive`.
- **`desktop/`** — Electron + React (spawna o daemon como sidecar).

Validado: detecção de drives sem admin (NVMe/SSD via PowerShell), tiering com
junction transparente, limpeza de deps + junk, journal, 19 testes, clippy limpo.

---

## 1. Princípios de design (não negociar ao evoluir)

1. **Segurança primeiro**: dry-run por padrão, journal de operações, nada destrutivo
   sem confirmação explícita.
2. **Tiering transparente**: mover + junction (caminho original continua válido).
3. **Atividade sem `atime`**: NTFS invalida last-access — usar mtime + último commit
   git (+ watcher, na Fase 3).
4. **Catálogo extensível de cleanup**: deps regeneráveis + junk de OS/editor/logs.
5. **Core isolado**: `opt-drive-core` não conhece rede/HTTP — fácil de testar.

---

## 2. Catálogo de limpeza ✅ (recém-entregue)

Cobre as principais linguagens + arquivos "inúteis", extensível via config.

- Linguagens (dirs): JavaScript/TS, Rust, Python, Java/JVM, C/C++, C#/.NET,
  Swift/Xcode, Elixir, Haskell, Dart, Lua, R, Scala, PHP.
- Arquivos (globs): `*.log`, `*.tmp`, `*.bak`, `*.pid`, `*.lock`, `*.swp`, `*.swo`,
  `*~`, `*.orig`, `*.rej`, `*.pyc`, `*.pyo`, `*.class`, `*.o`, `*.obj`, `*.a`,
  `.DS_Store`, `Thumbs.db`, `ehthumbs.db`, `desktop.ini`.
- Caches/cobertura: `coverage`, `.nyc_output`, `.idea`, `.vs`, `.history`.

### Estender o catálogo (usuário)
```toml
[cleanup]
use_default_catalog = true          # catálogo embutido

[[cleanup.targets]]                 # adiciona (soma ao embutido)
name = "meu-build"
patterns = ["build", "out"]
match_dirs = true

[[cleanup.targets]]
patterns = ["*.map"]                # arquivo: forma resumida (objeto)
match_files = true
category = "logs"
```
Ou forma simples (strings viram dirs, category="custom"):
```toml
[cleanup]
targets = ["build", ".cache"]
```

### Próximos passos do catálogo
- [x] Endpoint `GET /api/cleanup/catalog` (lista categorizada p/ UI). ✅
- [x] UI: accordion de cleanup agrupado por categoria (Linguagens / Sistema / Logs),
  com toggle por alvo, "selecionar tudo" por grupo e global. ✅
- [ ] Ação dedicada "limpar junk" (sem mover) + estimativa de espaço recuperado.
- [x] Alvos de `regenerable=false` exibidos com chip "cuidado" na UI. ✅

---

## 2.1 Explorer de drives + gauge de velocidade ✅

Melhorias na tela **Drives** e navegação de diretórios:

- **% livre** ao lado de usados/livres em cada card de drive.
- **Speed gauge** (anel SVG) substituindo o badge slow/fast: o preenchimento reflete a
  velocidade relativa (`speed_frac`, mapeamento não-linear NVMe/SSD/HDD) e a cor vem do
  tier. Hover abre **tooltip** com marca, modelo, interface, RPM e ~read/write MB/s
  (estimados por tipo — não medidos).
- **Marca do drive** (`brand`) além do modelo, derivada do `Manufacturer` do PowerShell
  ou do prefixo do modelo (Samsung, WD, Seagate/ST, Crucial/CT, …).
- **Seleção de drive → Explorer**: clicar num card abre abaixo um navegador de diretórios
  (breadcrumb, pastas antes de arquivos, tamanho de cada pasta e mtime).
  - A **estrutura** vem sempre do `read_dir` (instantâneo, sempre atual); o **tamanho de
    cada pasta** vem do índice SQLite (`dir_size`) quando indexada, senão mostra "—"
    (não há cálculo recursivo sob demanda — seria catastrófico na raiz de um drive).
    Arquivos sempre mostram o tamanho real do metadata. Indicador verde = indexado.
  - **Progresso de indexação** visível: CLI com barra `indicatif` (tempo real) e log
    periódico no terminal do daemon; UI já mostra pelo WS na sidebar.
- Endpoints novos: `GET /api/browse?path=`, `GET /api/cleanup/catalog`.
- Campos novos em `Drive`: `brand`, `read_mbps`, `write_mbps`, `speed_frac`.
- `config.cleanup.disabled` (blacklist de alvos do catálogo) — toggle individual sem
  perder o catálogo embutido.

> Nota: o **tiering** e a **indexação** já estavam implementados (aba Tiering + botão
> "Re-indexar" na sidebar). O estado vazio da aba Tiering agora explica os pré-requisitos
> (watch paths + indexar + projetos com `.git` inativos).

---

## 2.2 Indexação em tempo real (file-watcher) ✅

Indexação **incremental** que reage a mudanças do filesystem, sem precisar de re-scan
completo. Mantém o SQLite sempre sincronizado (create/modify/remove) — o explorer e o
tiering refletem o disco "agora".

- **Core** (`index/watch.rs`): lógica pura + testável, sem depender de `notify`.
  - `ChangeBatch` coalesce eventos por path (last-write-wins; rename = 2 paths).
  - `Indexer::apply_changes` decide, por evento, upsert vs. skip **espelhando o walker**
    (`ignore_globs`, arquivos junk `*.log`/`.DS_Store`, subárvores de cleanup). Um path
    sumido vira remoção; `IndexDb::apply_mixed` faz tudo numa transação (delete em árvore).
- **Daemon** (`watcher.rs`): `notify-debouncer-mini` observa `watch.paths` (recursivo) numa
  thread própria; cada lote debounced vai ao runtime tokio → `spawn_blocking` → core.
  Emite `Event::IndexUpdated` no WS.
  - O debouncer mini não distingue create/modify/remove — marcamos tudo Upsert e o core
    decide via `stat` (existe → upsert, sumiu → remove). Reflete o **estado final** real.
- **Config**: `[watch] realtime` (default `true`) + `debounce_ms` (default 600).
- **UI**: sidebar atualiza o contador ao receber `index_updated` (throttled); botão
  "Re-indexar" agora mostra erros (antes engolia silenciosamente — ex.: `watch.paths` vazio).
- **Bug de logging corrigido**: o filtro default (`opt_drive=info`) não casava com os alvos
    `opt-drive.*` (hífen) da convenção do AGENTS.md; agora inclui ambos os prefixos.
- 10 testes novos no core (29 no total); validado end-to-end via HTTP (create/delete/junk-skip/
  dir-tree). `notify`/`notify-debouncer-mini` já estavam no workspace mas não eram usados.

### Próximos passos do watcher
- [ ] Reiniciar o watcher ao salvar config com paths novos (hoje lê no boot do daemon).
- [ ] Seed inicial se o DB estiver vazio e `scan_interval = 0` (evitar índice parcial na 1ª run).
- [ ] `opt-drive watch` na CLI (wiring hoje é só no daemon).

---

## 2.3 Tela de Indexação (UI) ✅

Aba **🗂️ Indexação** no desktop (componente `Index.tsx` + wire no `App.tsx`):

- **Progresso ao vivo** durante a varredura, alimentado pelos eventos WS
  (`scan_started` → `scan_progress` → `scan_done`/`scan_canceled`): barra de progresso
  (determinada c/ % quando há `total_estimate` do último scan; indeterminada c/ pulso
  no 1º scan), **pasta atual** na ponta do scan, tiles de entradas, velocidade (entradas/s),
  decorrido, **ETA** (derivado de rate × restante), bytes e erros.
- Botões **▶ Iniciar varredura / ■ Cancelar**; o "Re-indexar" da sidebar desabilita
  durante o scan e navega para a aba.
- **Config de CPU**: campo "Threads de varredura" (`config.indexer.threads`, 0 = nº de
  CPUs) com save debounced (merge contra config fresca do servidor p/ não clobberar
  outras abas). O scan em si já é paralelo (`ignore::WalkParallel`, motor do ripgrep).
- Estado ocioso: entradas indexadas, última varredura e nº de watch paths.
- **Bug "tela preta" corrigido**: daemons antigos não têm a seção `indexer` no
  `/api/config` → `cfg.indexer.threads` lançava TypeError e o React desmontava a árvore
  inteira. Agora `indexer` é opcional em `types.ts` (tsc garante acesso opcional em todo
  o código) + `ErrorBoundary` no root mostra o erro em vez de tela preta. Ao mudar a
  config no Rust, **reconstruir o daemon** (`cargo build -p opt-drive-daemon`) — o
  Electron (dev) spawna de `target/debug`.
- Dev/teste: `?api=<url>` na query string ou `localStorage.optDrive.apiBase` sobrepõem a
  base URL do daemon (permite rodar o renderer fora do Electron, ex.: contra um stub).

---

## 3. Fase 2 — Backup / Sync Google Drive ⬜

Objetivo: backup/sync incremental de pastas selecionadas, com restore.

### 3.1 Setup OAuth2 ⬜
- [ ] Criar projeto no Google Cloud Console → OAuth client (Desktop app).
- [ ] Baixar `credentials.json` (NÃO versionar — já no `.gitignore`).
- [ ] Escolher crate: `google-drive3` (oficial-style) **ou** `drive-v3` + `oauth2`/`yup-oauth2`.
  - Recomendação inicial: `drive-v3` (mais leve) + fluxo loopback OAuth2.
- [ ] Implementar `providers/google_drive/auth.rs`: primeiro uso abre browser p/
  consent → captura code via `localhost:<porta>` redirect → grava `token.json`
  (refresh token persistente). Renovação automática via refresh.

### 3.2 Implementar o provider ⬜
- [ ] `providers/google_drive/mod.rs`: `GoogleDrive` implementando `BackupProvider`
  (`authenticate`, `status`, `sync_dir`).
- [ ] `status(local)`: consulta arquivo/pasta remoto por caminho (Drive API `files.list`
  com `q: name=… and trashed=false`).
- [ ] Hashing local: SHA-256 por arquivo (`sha2`) — base do sync incremental.
- [ ] Manifest de sync: tabela `backup_state(path, sha256, mtime, remote_id)` no SQLite.

### 3.3 Sync incremental ⬜
- [ ] Walk local (reusar `index::walker` c/ regras de cleanup p/ excluir junk).
- [ ] Comparar com `backup_state`: upload se hash mudou ou é novo.
- [ ] Upload **resumível** (Drive API multipart/resumable) p/ arquivos grandes.
- [ ] Dedup/pasta-app: usar uma pasta raiz `Opt-Drive/` no Drive do usuário.
- [ ] Políticas de exclusão remota: `--delete` opcional (default OFF — seguro).

### 3.4 Integração ⬜
- [ ] Config: seção `[backup]` (`provider = "google-drive"`, `paths = [...]`,
  `schedule`, `delete_remote = false`).
- [ ] Daemon scheduler: job periódico de backup (em janela configurada).
- [ ] API: `POST /api/backup/run`, `GET /api/backup/status`, eventos WS
  (`backup_progress`, `backup_done`).
- [ ] CLI: `opt-drive backup run` / `opt-drive backup status` / `opt-drive backup restore <id>`.
- [ ] UI: aba **Backup** (status, últimas execuções, botão "sincronizar agora").

### 3.5 Critério de pronto da Fase 2
- [ ] OAuth 1ª vez funciona; token persiste entre reinícios.
- [ ] Alterar um arquivo local e rodar sync → só ele é re-enviado.
- [ ] Restore de um arquivo via CLI.
- [ ] Estado de sync sobrevive a re-indexação.

---

## 4. Fase 3 — Refinamentos ⬜

### 4.1 Automação / residência
- [ ] Auto-start do daemon no boot (Windows: Task Scheduler / serviço).
- [ ] Tray icon (Electron `Tray`) com status e ações rápidas.
- [ ] Notificações nativas (backup concluído, espaço baixo, tiering aplicado).

### 4.2 Indexação / uso em tempo real
- [x] File-watcher `notify` + `notify-debouncer-mini`: índice incremental (sem re-scan total). ✅
- [ ] Tracking real de "acesso" (abrir arquivos) → score de atividade mais fiel que mtime.
- [ ] Frecência: contagem de modificações por janela temporal → promoção p/ fast tier.

### 4.3 Operações / segurança
- [ ] **Undo** real a partir do journal (reverter Relocate: move de volta + remove junction).
- [ ] Restore de `.tar.zst` (archive tier → descomprimir de volta).
- [ ] Agendamento de tiering em janela (ex.: só à noite) via cron-like no scheduler.
- [ ] Quotas: limitar quanto do drive lento/rápido o tiering pode ocupar.

### 4.4 Mais providers de backup
- [ ] OneDrive (Graph API), Dropbox, S3/REST genérico — todos via `BackupProvider`.
- [ ] Backup local (espelhamento entre drives, sem nuvem).

### 4.5 UX
- [ ] Gráficos de uso de espaço/histórico (recharts).
- [ ] Timeline de operações (do journal).
- [ ] Configuração assistida (wizard de 1ª execução: escolher drives, paths, regras).
- [ ] Empacotamento: `electron-builder` p/ installer `.exe` (bundle do daemon em `resources/bin`).

---

## 5. Backlog / ideias
- Deduplicação de conteúdo entre tiers (content-defined chunking).
- Detecção de duplicatas (arquivos idênticos em vários projetos).
- Integração com `.gitignore` real (hoje usamos `ignore_globs` simples).
- Suporte a Linux/macOS na detecção de drives (hoje focado em Windows).
- Modo "somente leitura / auditoria" (relatórios sem aplicar nada).

---

## 6. Como executar uma tarefa (fluxo padrão)

1. **Entenda o módulo** — leia `AGENTS.md` e o `README.md`.
2. **Adicione um teste** que captura o comportamento esperado (TDD quando fizer sentido).
3. **Implemente** mantendo `cargo test` verde e `cargo clippy --all-targets` sem warnings.
4. **Smoke-test** de ponta a ponta (CLI e/ou daemon+UI) quando envolver I/O.
5. **Atualize este `plan.md`** (marque ✅) e o `README.md` se a interface mudar.

### Comandos úteis
```bash
cargo test                              # 19 testes unitários
cargo clippy --all-targets             # deve estar sem warnings
cargo run -p opt-drive-cli -- drives   # valida detecção de drives
cd desktop && npm run dev              # app completo (electron + daemon)
```

### Decisões pendentes (a decidir ao iniciar a Fase 2)
- `google-drive3` vs `drive-v3` (avaliar ergonomia de tipos e manutenção).
- Estratégia de rename/move detection no sync (Drive `files.update` com `addParents`).
- Política padrão de retenção/exclusão remota.
