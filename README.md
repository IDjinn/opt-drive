# Opt-Drive

Gerenciamento inteligente de arquivos e backups para múltiplos drives.

Move automaticamente projetos/arquivos inativos do SSD rápido (NVMe) para o SSD/HDD
mais lento, limpa dependências regeneráveis (`node_modules`, `target`, …) durante o
tiering, compacta o que for arquivar, e (na fase 2) faz backup/sync com Google Drive.

> 📄 **Docs**: [`plan.md`](./plan.md) (roadmap passo a passo) ·
> [`AGENTS.md`](./AGENTS.md) (guia para contribuir / agentes).

## Arquitetura

```
Electron + React  ◄──HTTP/WS──►  opt-drive daemon (Rust)  ──►  filesystem
                                       │  usa
                            opt-drive-core (lib: lógica de domínio)

opt-drive-cli (bin) ─────────────────► opt-drive-core  (automação / cron)
```

| Crate / app | Função |
|---|---|
| `crates/opt-drive-core` | Toda a lógica: detecção de drives, indexação (SQLite), regras de tiering, operações (mover+junction, limpeza, compressão), trait de providers de backup. |
| `crates/opt-drive-daemon` | Serviço residente: scheduler + API REST/WebSocket em `127.0.0.1` (porta efêmera). |
| `crates/opt-drive-cli` | Linha de comando (`opt-drive`) para automação/manual. |
| `desktop/` | App Electron + React (UI). Spawna o daemon como sidecar e descobre a porta via stdout. |

### Detecção de drives (sem admin)

No Windows, espaço/rótulo vêm das APIs Win32; o **tipo** (NVMe/SSD/HDD + bus type)
vem dos cmdlets `Get-Disk`/`Get-PhysicalDisk` via PowerShell — então **não exige
privilégio de administrador** (abrir `\\.\C:` com IOCTL exigiria). Os tiers são
classificados: NVMe→fast, SSD→slow, HDD/removível→archive. O usuário também pode
forçar tiers por drive no `config.toml`.

## Princípios de segurança

- **Dry-run por padrão**: nenhuma operação destrutiva roda sem `--apply` ou botão na UI.
- **Journal de operações**: todo lote aplicado grava um manifest JSON (auditoria).
- **Tiering transparente**: ao mover um projeto, um *junction* no caminho original
  mantém o caminho funcionando — os bytes vão para o drive lento, mas
  `C:\dev\proj` continua válido.
- **Atividade sem `atime`**: o NTFS desabilita last-access por padrão (e, quando
  habilitado, é atualizado por qualquer leitura — inclusive o próprio scan, o que
  invalida o sinal). O scorer usa **mtime + data do último commit git**; o daemon
  mantém o índice sempre atual via file-watcher (`notify`), então mtime reflete o
  uso real sem precisar de last-access.

## Catálogo de limpeza (extensível)

Na hora de mover/comprimir, o opt-drive descarta **dependências regeneráveis** e
**arquivos "inúteis"** (não os move). O catálogo embutido cobre as principais
linguagens e é **extensível** via config:

- **Linguagens (dirs)**: JS/TS (`node_modules`, `.next`, `.nuxt`, …), Rust (`target`),
  Python (`.venv`, `__pycache__`, …), Java/JVM, C/C++, C#/.NET, Swift/Xcode, Elixir,
  Haskell, Dart, Lua, R, Scala, PHP.
- **Arquivos (globs)**: `*.log`, `*.tmp`, `*.bak`, `*.pid`, `*.swp`, `*~`, `*.orig`,
  `*.pyc`, `*.class`, `*.o`, `*.obj`, `.DS_Store`, `Thumbs.db`, `desktop.ini`, …
- **Caches/IDE**: `.idea`, `.vs`, `.history`, `coverage`, `.nyc_output`.

> Binários "shippable" (`*.dll`/`*.exe`/`*.so`) são propositalmente **excluídos** do
> catálogo padrão para não apagar entregáveis do projeto.

Para adicionar/customizar ( soma ao catálogo embutido quando `use_default_catalog = true`):

```toml
[cleanup]
use_default_catalog = true

[[cleanup.targets]]
name = "meu-build"
patterns = ["build", "out"]
match_dirs = true

[[cleanup.targets]]
patterns = ["*.map"]
match_files = true
category = "logs"

# ou forma simples (vira dir, category="custom"):
# targets = ["build", ".cache"]
```

## Como rodar

### Pré-requisitos
- Rust (stable) e Node.js 18+.

### Rust (core / cli / daemon)
```bash
cargo build          # compila os 3 crates
cargo test           # 11 testes unitários (walker, policy, compress, cleanup, catálogo, …)
cargo clippy --all-targets   # sem warnings
```

### CLI
```bash
cargo run -p opt-drive-cli -- init          # cria config padrão
cargo run -p opt-drive-cli -- drives        # lista drives + tiers
cargo run -p opt-drive-cli -- scan          # indexa watch paths
cargo run -p opt-drive-cli -- status        # stats do índice
cargo run -p opt-drive-cli -- tier          # PREVIEW (dry-run) do tiering
cargo run -p opt-drive-cli -- tier --apply  # executa o tiering
cargo run -p opt-drive-cli -- config show
```

### Daemon (direto)
```bash
cargo run -p opt-drive-daemon -- --port 0
# imprime: OPTDRIVE_LISTENING {"port":NNNN,"host":"127.0.0.1"}
```

### Desktop (Electron + React)
```bash
cd desktop
npm install
npm run dev     # vite + electron; o electron spawna o daemon (target/debug)
```
> Se o `npm install` (Node 24+) bloquear os postinstalls do electron/esbuild por
> segurança, rode `node node_modules/esbuild/install.js` e
> `node node_modules/electron/install.js` (ou extraia o zip do cache para `dist/`).

## API do daemon

- `GET /api/health` · `GET /api/drives`
- `GET/PUT /api/config`
- `POST /api/index/run` · `GET /api/index/status`
- `GET /api/browse?path=<abs>` — lista diretórios/arquivos com tamanho (explorer híbrido)
- `GET /api/cleanup/catalog` — catálogo de cleanup (builtin + custom) p/ a UI
- `GET /api/tier/preview` · `POST /api/tier/apply`
- `GET /api/journals`
- `WS /api/events` — progresso de scan/tiering ao vivo + `index_updated` (incremento do watcher)

## Configuração (`config.toml`)

Local padrão (Windows): `%APPDATA%\optdrive\opt-drive\config.toml`. Exemplo:

```toml
[[drives]]
path = "C:\\"
tier = "fast"

[[drives]]
path = "D:\\"
tier = "slow"

[watch]
paths = ["C:\\dev"]
ignore_globs = ["**/target/**", "**/.git/**"]
realtime = true                    # indexação incremental em tempo real (file-watcher)
debounce_ms = 600                  # janela de agrupamento de bursts (npm install, cargo build)

[cleanup]
use_default_catalog = true          # catálogo embutido (linguagens + junk)
# targets = [...]                   # adicionais custom (ver seção "Catálogo de limpeza")

[[rules]]
name = "projetos-inativos"
match_glob = "**/*"
inactive_days = 30
from_tier = "fast"
to_tier = "slow"
cleanup_deps = true
junction = true
```

## Status

- [x] **Fase 1 — motor de tiering (MVP)**: detecção de drives, indexação, regras,
  mover+junction, limpeza de deps, compressão, CLI, daemon (REST+WS), UI Electron.
- [x] **Catálogo de limpeza extensível**: principais linguagens + junk de OS/editor/logs,
  configurável e expansível via `config.toml`.
- [x] **Indexação em tempo real**: file-watcher `notify` aplica mudanças incrementais ao
  índice (create/modify/remove) sem re-scan completo — o explorer e o tiering ficam
  sempre atualizados.
- [ ] **Fase 2 — backup / Google Drive**: `BackupProvider` (OAuth, sync incremental).
- [ ] **Fase 3 — refinamento**: UI polida, mais providers, auto-start, restore guiado.
