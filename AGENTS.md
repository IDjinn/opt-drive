# AGENTS.md — Guia para agentes e contribuidores

Instruções para trabalhar neste repositório (humanos ou agentes de IA). Leia antes
de tocar no código.

## Visão geral

Opt-Drive é um gerenciador de arquivos/backups com **tiering automático entre drives**
(mover projetos inativos do SSD rápido → lento, preservando o caminho via junction) e
**backup/sync** (inicialmente Google Drive). Plataforma-alvo primária: **Windows**.

Arquitetura: **Electron + React** ⇄ **daemon Rust (REST/WS)** ⇄ **filesystem**. O
`opt-drive-core` (lib) contém toda a lógica e não conhece rede/HTTP.

## Mapa do repositório

```
Cargo.toml                     # workspace
crates/
  opt-drive-core/              # LÓGICA (sem rede) — onde 90% das regras vivem
    src/
      config/mod.rs            #   modelo de config (TOML) + CleanupConfig
      cleanup_catalog.rs       #   catálogo de cleanup (linguagens + junk) + CleanupRules
      drives/{mod,windows,other}.rs  # detecção/classificação de drives
      index/{mod,db,walker}.rs #   varredura + SQLite
      usage/mod.rs             #   score de atividade (mtime + git commit)
      policy/mod.rs            #   engine de regras → Plan de Actions
      ops/{mod,relocate,cleanup,compress,journal}.rs  # execução c/ undo
      providers/mod.rs         #   trait BackupProvider (+ google_drive stub)
  opt-drive-daemon/            # axum REST + WS + scheduler; usa core
  opt-drive-cli/               # clap; usa core
desktop/                       # Electron (electron/main.js, preload.js) + React (src/)
```

## Invariantes (NÃO quebre)

1. **Dry-run por padrão.** Nenhuma operação que move/apaga roda sem `--apply` /
   confirmação explícita. Toda ação destrutiva passa pelo `ops::Executor` e grava um
   `ops::journal::Journal`.
2. **Junction transparente.** Ao mover um projeto, recrie o caminho original como
   junction apontando para o destino (`ops::relocate::create_junction`). Caso
   contrário, os caminhos quebram.
3. **Não confie em `atime`.** NTFS desabilita last-access por padrão e, quando ligado,
   é atualizado por qualquer leitura (inclusive o scan). Score de atividade usa
   **mtime + último commit git** (`usage::ActivityScorer`).
4. **Drive detection sem admin.** No Windows, NÃO abra `\\.\C:` com IOCTL (dá
   ERROR_ACCESS_DENIED). Use PowerShell `Get-Disk`/`Get-PhysicalDisk` (em
   `drives/windows.rs`). APIs Win32 (espaço/rótulo) são OK.
5. **Core isolado de I/O de rede.** `opt-drive-core` não faz HTTP — daemon e CLI
   orquestram. Isso mantém o core testável.

## Convenções de código

- **Erros**: `anyhow::Result` em código de app (daemon/cli); `thiserror` só se precisar
  de tipos de erro públicos em libs. Handlers do axum usam `AppError`
  (`api.rs`) que vira HTTP 500.
- **Async**: `tokio`. Operações pesadas (scan, plan, apply) em `spawn_blocking` (o
  SQLite é síncrono). Serializar jobs pesados com `state.run_lock`.
- **Serde**: tipos de domínio derivam `Serialize, Deserialize` (a API fala JSON e a
  config fala TOML). Campos opcionais com `#[serde(default)]`.
- **Paths**: funções recebem `&Path` (não `&PathBuf`). `drives::mount_of` normaliza para
  `"X:\\"` (mesmo formato de `Drive.mount`).
- **Globs de cleanup**: casam contra o **nome** do arquivo/dir (não o caminho). Use
  `CleanupRules` (compila globsets uma vez) — nunca faça match string-a-string em hot path.
- **Logging**: `tracing` com alvo `opt-drive.<módulo>` (ex.: `opt-drive.cleanup`).
- **Comentários**: em português, densidade igual à do entorno.

## Como adicionar X

### Um alvo de cleanup (nova linguagem/junk)
Edite `cleanup_catalog::default_catalog()` em `cleanup_catalog.rs`:
```rust
CleanupTarget::dirs("zigger-build", &["zig-cache", "zig-out"], "zig"),
CleanupTarget::files("zigger-tmp", &["*.zig.cpp"], "zig"),
```
Adicione o nome a `cleanup_catalog::tests::catalog_has_major_languages`. Pronto —
walker e `ops::cleanup` já usam `CleanupRules` automaticamente.

### Um endpoint de API (daemon)
Em `daemon/src/api.rs`: adicione `async fn handler(State(st): State<AppState>) -> R<T>`,
registre em `router()`. Operação pesada? Envolva em `spawn_blocking` e emita um
`state::Event`. Adicione o tipo DTO em `desktop/src/types.ts` e o método em `api.ts`.

### Um comando de CLI
Em `cli/src/main.rs`: adicione variante em `Cmd` (clap derive), despache em `main`,
reaproveite `core`. Mantenha saída legível (use `fmt_bytes`).

### Um provider de backup
Implemente `providers::BackupProvider` (`name`, `authenticate`, `status`, `sync_dir`).
Veja `providers/mod.rs` e o stub `google_drive`. Depois conecte à config/scheduler/API.

### Uma tela de UI
Em `desktop/src/components/`: crie o componente, adicione aba em `App.tsx`, consuma
`api.ts`. Mantenha o tema escuro (`styles.css`).

## Testes & qualidade

```bash
cargo test                         # deve passar (atualmente 11 testes)
cargo clippy --all-targets         # deve estar SEM warnings
cd desktop && npx tsc --noEmit     # renderer deve tipar
```
- Testes de filesystem usam `tempfile::tempdir()`.
- Sempre escreva um teste para: nova regra de policy, novo alvo de cleanup, nova
  transformação de paths, e qualquer lógica de matching glob.

## Fluxo recomendado ao pegar uma tarefa

1. Leia o `plan.md` (estado + próximos passos) e a seção relevante deste arquivo.
2. Reproduza localmente o comportamento atual (CLI/daemon) antes de mudar.
3. Pequenos passos + `cargo test`/`cargo clippy` a cada mudança.
4. Para I/O de disco (mover/apagar), **sempre** teste com fixture temporário e confirme
   via journal; nunca teste destrutivamente em dados reais do usuário.
5. Atualize `plan.md` (marque ✅) e `README.md` se a interface/config mudar.

## Armadilhas conhecidas

- **`rm -rf` em junction no Git Bash** segue o link e apaga o **alvo** — para remover só
  o junction use `cmd //c rmdir <caminho>` (ou `ops::relocate::remove_junction`).
- **Postinstall do Electron/Esbuild** (Node 24+) podem ser bloqueados pelo npm — rode
  `node node_modules/electron/install.js` (ou extraia o zip do cache para `dist/`).
- **`GetLastError`** deve ser lido **imediatamente** após a chamada Win32 — qualquer
  chamada intermediária (ex.: `env::var`) zera o last-error.
- **robocopy**: códigos de saída `< 8` são sucesso (não confunda com `0`).
