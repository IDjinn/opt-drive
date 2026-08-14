# Opt-Drive

Intelligent file and backup management across multiple drives.

Automatically moves inactive projects/files from the fast (NVMe) SSD to the slower
SSD/HDD, cleans regenerable dependencies (`node_modules`, `target`, …) during
tiering, compresses whatever is being archived, and (in phase 2) performs
backup/sync with Google Drive.

> 📄 **Docs**: [`plan.md`](./plan.md) (step-by-step roadmap) ·
> [`AGENTS.md`](./AGENTS.md) (contribution guide / agents).

## Architecture

```
Electron + React  ◄──HTTP/WS──►  opt-drive daemon (Rust)  ──►  filesystem
                                       │  uses
                            opt-drive-core (lib: domain logic)

opt-drive-cli (bin) ─────────────────► opt-drive-core  (automation / cron)
```

| Crate / app | Role |
|---|---|
| `crates/opt-drive-core` | All the logic: drive detection, indexing (SQLite), tiering rules, operations (move+junction, cleanup, compression), backup provider trait. |
| `crates/opt-drive-daemon` | Resident service: scheduler + REST/WebSocket API on `127.0.0.1` (ephemeral port). |
| `crates/opt-drive-cli` | Command line (`opt-drive`) for automation/manual use. |
| `desktop/` | Electron + React app (UI). Spawns the daemon as a sidecar and discovers the port via stdout. |

### Drive detection (no admin required)

On Windows, free space/label come from Win32 APIs; the **type** (NVMe/SSD/HDD +
bus type) comes from the `Get-Disk`/`Get-PhysicalDisk` cmdlets via PowerShell —
so it **does not require administrator privileges** (opening `\\.\C:` with an
IOCTL would). Tiers are classified as: NVMe→fast, SSD→slow, HDD/removable→archive.
The user can also force tiers per drive in `config.toml`.

## Safety principles

- **Dry-run by default**: no destructive operation runs without `--apply` or a UI button.
- **Operation journal**: every applied batch writes a JSON manifest (auditing).
- **Transparent tiering**: when a project is moved, a *junction* at the original
  path keeps the path working — the bytes go to the slow drive, but
  `C:\dev\proj` remains valid.
- **Activity without `atime`**: NTFS disables last-access by default (and, when
  enabled, it is updated by any read — including the scan itself, which
  invalidates the signal). The scorer uses **mtime + last git commit date**; the
  daemon keeps the index up to date via a file watcher (`notify`), so mtime
  reflects real usage without needing last-access.

## Cleanup catalog (extensible)

When moving/compressing, opt-drive discards **regenerable dependencies** and
**"useless" files** (it doesn't move them). The built-in catalog covers the major
languages and is **extensible** via config:

- **Languages (dirs)**: JS/TS (`node_modules`, `.next`, `.nuxt`, …), Rust (`target`),
  Python (`.venv`, `__pycache__`, …), Java/JVM, C/C++, C#/.NET, Swift/Xcode, Elixir,
  Haskell, Dart, Lua, R, Scala, PHP.
- **Files (globs)**: `*.log`, `*.tmp`, `*.bak`, `*.pid`, `*.swp`, `*~`, `*.orig`,
  `*.pyc`, `*.class`, `*.o`, `*.obj`, `.DS_Store`, `Thumbs.db`, `desktop.ini`, …
- **Caches/IDE**: `.idea`, `.vs`, `.history`, `coverage`, `.nyc_output`.

> "Shippable" binaries (`*.dll`/`*.exe`/`*.so`) are intentionally **excluded**
> from the default catalog so project deliverables are never deleted.

To add/customize (adds to the built-in catalog when `use_default_catalog = true`):

```toml
[cleanup]
use_default_catalog = true

[[cleanup.targets]]
name = "my-build"
patterns = ["build", "out"]
match_dirs = true

[[cleanup.targets]]
patterns = ["*.map"]
match_files = true
category = "logs"

# or the simple form (becomes a dir target, category="custom"):
# targets = ["build", ".cache"]
```

## Running

### Prerequisites
- Rust (stable) and Node.js 18+.

### Rust (core / cli / daemon)
```bash
cargo build          # builds the 3 crates
cargo test           # 11 unit tests (walker, policy, compress, cleanup, catalog, …)
cargo clippy --all-targets   # no warnings
```

### CLI
```bash
cargo run -p opt-drive-cli -- init          # creates default config
cargo run -p opt-drive-cli -- drives        # list drives + tiers
cargo run -p opt-drive-cli -- scan          # index watch paths
cargo run -p opt-drive-cli -- status        # index stats
cargo run -p opt-drive-cli -- tier          # tiering PREVIEW (dry-run)
cargo run -p opt-drive-cli -- tier --apply  # run the tiering
cargo run -p opt-drive-cli -- config show
```

### Daemon (directly)
```bash
cargo run -p opt-drive-daemon -- --port 0
# prints: OPTDRIVE_LISTENING {"port":NNNN,"host":"127.0.0.1"}
```

### Desktop (Electron + React)
```bash
cd desktop
npm install
npm run dev     # vite + electron; electron spawns the daemon (target/debug)
```
> If `npm install` (Node 24+) blocks the electron/esbuild postinstalls for
> security reasons, run `node node_modules/esbuild/install.js` and
> `node node_modules/electron/install.js` (or extract the zip from the cache
> into `dist/`).

## Daemon API

- `GET /api/health` · `GET /api/drives`
- `GET/PUT /api/config`
- `POST /api/index/run` · `GET /api/index/status`
- `GET /api/browse?path=<abs>` — lists directories/files with size (hybrid explorer)
- `GET /api/cleanup/catalog` — cleanup catalog (builtin + custom) for the UI
- `GET /api/tier/preview` · `POST /api/tier/apply`
- `GET /api/journals`
- `WS /api/events` — live scan/tiering progress + `index_updated` (watcher increment)

## Configuration (`config.toml`)

Default location (Windows): `%APPDATA%\optdrive\opt-drive\config.toml`. Example:

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
realtime = true                    # real-time incremental indexing (file watcher)
debounce_ms = 600                  # burst grouping window (npm install, cargo build)

[cleanup]
use_default_catalog = true         # built-in catalog (languages + junk)
# targets = [...]                  # custom additions (see "Cleanup catalog" section)

[[rules]]
name = "inactive-projects"
match_glob = "**/*"
inactive_days = 30
from_tier = "fast"
to_tier = "slow"
cleanup_deps = true
junction = true
```

## Status

- [x] **Phase 1 — tiering engine (MVP)**: drive detection, indexing, rules,
  move+junction, dependency cleanup, compression, CLI, daemon (REST+WS), Electron UI.
- [x] **Extensible cleanup catalog**: major languages + OS/editor/log junk,
  configurable and expandable via `config.toml`.
- [x] **Real-time indexing**: `notify` file watcher applies incremental changes to
  the index (create/modify/remove) without a full re-scan — the explorer and
  tiering stay always up to date.
- [ ] **Phase 2 — backup / Google Drive**: `BackupProvider` (OAuth, incremental sync).
- [ ] **Phase 3 — polish**: polished UI, more providers, auto-start, guided restore.
