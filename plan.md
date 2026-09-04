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
- [x] Reiniciar o watcher ao salvar config com paths novos (`PUT /api/config` para o
  watcher antigo via `WatcherHandle` e inicia um novo com a config vigente). ✅
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

## 2.4 Robustez do daemon/UI + caminhos protegidos + gerência de drives ✅

Correções de bugs reportados na prática (explorer não navegava, tiering em
skeleton eterno) e recursos novos:

- **Cache de enumeração de drives** (30s TTL, aquecido no boot): no Windows a
  enumeração spawna PowerShell (`Get-Disk`, 1-10s) — `/api/browse` pagava isso a
  cada clique, e cliques rápidos empilhavam processos (a listagem nunca chegava).
  Agora `AppState::cached_drives` serve do cache e renova em `spawn_blocking`.
- **`/api/index/run` como job em background**: responde `started` na hora (antes
  pendurava a conexão HTTP pelo scan inteiro), responde **409** se outro job
  pesado roda (antes esperava na fila do `run_lock`), e emite evento terminal em
  todos os caminhos — novo `scan_failed` (antes erro no meio do scan deixava a UI
  presa em "Indexando…" para sempre; idem no scheduler).
- **Tiering estável**: `index_updated` do WS agora throttle de 3s no
  `refreshKey` (antes cada evento cancelava o fetch do `/api/tier/preview` em
  andamento — skeleton eterno durante atividade do watcher); fetch com timeout
  de 30s (AbortSignal) em toda a API; `tierApply` consumia resposta com shape
  errado (`{report}` vs `RunReport` puro → TypeError após apply bem-sucedido).
- **Caminhos protegidos** (novo `core::protected`): dirs de sistema na raiz do
  drive (`Windows`, `Program Files`, `$RECYCLE.BIN`, …), arquivos de sistema
  (`pagefile.sys`, `ntuser.dat`, …), pastas de cloud-sync (`Google Drive`,
  `OneDrive`*, `Dropbox`) e reparse points (junctions/symlinks). Guardas em 4
  pontos: walker (nunca indexa dirs de sistema na raiz — mesmo com
  `watch.paths=["C:\\"]`), policy (nunca planeja ações neles), executor (recusa
  com erro no journal) e cleanup (não coleta reparse points). Extras do usuário
  via `protected_paths` na config. Explorer marca com 🔒.
- **Paths verbatim normalizados**: `canonicalize()` retorna `\\?\C:\...`, mas as
  chaves do índice são plain — os tamanhos de pasta do explorer nunca batiam.
  `browse::normalize_verbatim` corrige (incl. `\\?\UNC\`).
- **Gerência de drives na aba Drives**: cada card tem toggle **Indexar**
  (adiciona/remove o mount em `watch.paths`) + chips das subpastas monitoradas
  (remover individual) + seletor de **tier** (Auto/Rápido/Médio/Arquivo →
  `config.drives` override). Salvar config reinicia o watcher (ver 2.2).
- "Indexar agora" do Explorer desabilita durante scan e mostra erros (antes
  engolia); `atRoot` case-insensitive; check de `..` por componente (aceita
  `my..folder`).
- Testes: 50 no total (protected matching, policy skip, verbatim strip).

---

## 2.5 Lock do SQLite + cache da raiz dos drives ✅

- **"database is locked" nos incrementos** (logs do watcher), versão final: o daemon
  agora mantém **uma única conexão de escrita** (`AppState::index_db:
  Arc<IndexDb>`) compartilhada por TODOS os writers — watcher, `/api/index/run`,
  scheduler de scan (1h) e schedulers/handlers de backup. Como o lock de escrita
  do SQLite é por banco e só é disputado entre **conexões** concorrentes, uma
  conexão única o torna estruturalmente impossível dentro do processo: os writers
  fazem fila no `Mutex` interno, sem timeout e sem perder lotes. Para tanto,
  `scan`/`apply_changes` migraram do wrapper `Indexer` para métodos de `IndexDb`
  (o wrapper foi removido; CLI e testes atualizados). Leituras (browse, status,
  tier preview) continuam abrindo conexões próprias por request — em WAL,
  leitores nunca bloqueiam writers.
- Defesas anteriores mantidas: `PRAGMA busy_timeout=5000` em `IndexDb::open`
  (cobre o caso multi-processo: CLI rodando junto do daemon) + teste
  `busy_timeout_espera_writer_concorrente`; watcher com consumidor único
  (`tokio::mpsc`) que aplica lotes debounced em série (antes: um spawn por lote,
  cada um com conexão própria).
- **Smoke test manual**: watcher ativo + burst de 900+ criações/remoções + dois
  scans completos concorrentes → 0 ocorrências de "database is locked", 0 WARNs,
  índice consistente (contagem bateu com o estado final do disco).
- **Timeout de 30s no `/api/browse`** (apareceu depois que o índice passou a cobrir
  drives inteiros): `dir_size` consultava com `LIKE 'path\%'`, que é
  case-insensitive e portanto **nunca usava o índice da PK** — cada pasta listada
  custava um scan completo da tabela; a raiz de um drive (dezenas de pastas ×
  milhões de linhas) estourava o timeout do frontend (e o prefetch da raiz dos
  drives disparava isso pra todos os drives no boot). Reescrito como **range scan
  indexado** (`path >= ? AND path < path+\u{10FFFF}`), que toca só as linhas da
  subárvore; `SQL_DELETE_TREE` (remove_tree/apply_mixed) recebeu o mesmo
  tratamento. Bench (release, 500k linhas): subpasta 2,4ms · raiz 117ms. Smoke:
  browse ≤5ms com scan completo + churn simultâneos. Contrapartida aceita: a
  comparação agora é case-sensitive — pasta renomeada só em caso volta a ter
  tamanho após o próximo incremento/scan.
- Nota de launch: o Electron spawna `target/debug/opt-drive-daemon.exe` **sem
  rebuild** — depois de mudanças no Rust, rodar `cargo build -p opt-drive-daemon`
  (ou `run.bat`) antes de reiniciar o app, senão o binário antigo roda.
- **Raiz dos drives sempre em cache no desktop**: `Drives` semeia
  `browse:<mount>` no cache in-memory quando a lista de drives chega — o primeiro
  clique num card pinta o Explorer na hora, sem skeleton de primeiro acesso.
  Testes: 51 no total.

---

## 3. Fase 2 — Backup / Sync multi-conector + Encriptação ✅

Objetivo: backup/sync incremental de pastas selecionadas, com restore, múltiplos
conectores (local/S3/Google Drive) e encriptação opcional dos arquivos.

Arquitetura entregue:

- **Core** (`opt-drive-core`): trait `BackupProvider` + motor incremental
  (`providers/sync.rs` — walk, SHA-256 por arquivo, manifest `backup_state` no
  SQLite, só re-envia o que mudou), encriptação `ops/encrypt.rs` (ChaCha20-Poly1305
  + Argon2, formato `.odenc`), config `[backup]`.
- **Conectores** (`opt-drive-connectors`, crate novo — mantém o core sem rede):
  - `local.rs` — mirror entre drives/pastas (sem rede).
  - `s3.rs` — S3 e compatíveis (MinIO, R2, B2) via REST + SigV4 próprio (`sigv4.rs`).
  - `google_drive.rs` — OAuth2 loopback (browser + `localhost:<porta>/callback`),
    token persistente em `google_token.json`, renovação via refresh, Drive v3 REST.
  - Factory `connector_from_config` por string `connector`.
- **Daemon**: `POST /api/backup/run` (run_lock + spawn_blocking + WS
  `backup_started/progress/done`), `GET /api/backup/status`, `POST /api/backup/restore`,
  scheduler de backup (`schedule_secs > 0`).
- **CLI**: `backup run|status|restore <remote_id> <dst>`, `encrypt`/`decrypt`
  (dry-run default, `--apply`).
- **UI**: aba ☁️ Backup (status do conector, "Executar agora" com progresso WS).

### 3.x Config de backup
```toml
[backup]
connector = "s3"          # "local" | "s3" | "google-drive"
paths = ["C:\\dev"]
schedule_secs = 0         # 0 = só manual
delete_remote = false
encrypt = true

[backup.options]          # por conector:
bucket = "my-backups"     #   s3: bucket, region, endpoint, (ou env AWS_*)
region = "us-east-1"
#   local: target_root = "D:\\Backup"
#   google-drive: credentials_path = "C:\\secrets\\client_secret.json"

[backup.encryption]
passphrase_env = "OPT_DRIVE_PASSPHRASE"   # passphrase NUNCA fica na config
```

### Próximos passos do backup
- [ ] Upload resumível p/ arquivos grandes (hoje multipart/PUT único).
- [ ] `--delete` remoto real (flag já existe no motor; UI/CLI não expõem listagem).
- [ ] Testes de integração S3 contra MinIO local (`#[ignore]`).
- [ ] Rename/move detection (Drive `files.update addParents`).

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
- [x] S3/compatíveis (MinIO, R2, B2) via SigV4 próprio. ✅
- [x] Google Drive (OAuth2 loopback + Drive v3). ✅
- [x] Backup local (espelhamento entre drives, sem nuvem). ✅
- [x] Encriptação de arquivos (`.odenc`, ChaCha20-Poly1305 + Argon2) no pipeline
  de backup e via CLI (`opt-drive encrypt/decrypt`). ✅
- [ ] OneDrive (Graph API), Dropbox — todos via `BackupProvider`.

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
cargo test                              # 50 testes unitários
cargo clippy --all-targets             # deve estar sem warnings
cargo run -p opt-drive-cli -- drives   # valida detecção de drives
cd desktop && npm run dev              # app completo (electron + daemon)
```

### Decisões resolvidas (Fase 2)
- SDK do Drive descartado: OAuth2 + Drive v3 REST direto via `reqwest` blocking.
- S3 sem SDK oficial: SigV4 implementado em `opt-drive-connectors/src/sigv4.rs`.
- Rede fora do core: crate `opt-drive-connectors` (invariante 5 preservada).
