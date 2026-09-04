# Opt-Drive

Intelligent file and backup management across multiple drives.

Automatically moves inactive projects/files from the fast (NVMe) SSD to the slower
SSD/HDD, cleans regenerable dependencies (`node_modules`, `target`, …) during
tiering, compresses whatever is being archived, and performs **encrypted
incremental backups** to local mirrors, S3 (and S3-compatible: MinIO, R2, B2) or
Google Drive.

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
| `crates/opt-drive-core` | All the logic: drive detection, indexing (SQLite), tiering rules, operations (move+junction, cleanup, compression), backup sync engine, encryption (`.odenc`). |
| `crates/opt-drive-connectors` | Backup connectors (network lives here, not in core): local mirror, S3 (own SigV4 signing), Google Drive (OAuth2 loopback). |
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

### Quick start (Windows)
```bat
run.bat
```
One-shot launcher at the repo root: builds the Rust workspace if
`target\debug\opt-drive-daemon.exe` is missing, runs `npm install` on first
use, repairs the electron/esbuild postinstalls when Node blocks them, then
starts the desktop app (vite + electron; electron spawns the daemon as a
sidecar). Use `run.bat rebuild` to force a `cargo build` first.

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
cargo run -p opt-drive-cli -- backup run      # incremental backup (configured connector)
cargo run -p opt-drive-cli -- backup status
cargo run -p opt-drive-cli -- backup restore <remote_id> <dst>
cargo run -p opt-drive-cli -- encrypt <path> --apply   # local .odenc encryption (dry-run by default)
cargo run -p opt-drive-cli -- decrypt <path> --apply
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
- `POST /api/index/run` · `GET /api/index/status` · `POST /api/index/cancel`
  (`run` responds immediately with `202 {"started":true}` or `409` when another
  heavy job is in progress; progress arrives over the WebSocket)
- `GET /api/browse?path=<abs>` — lists directories/files with size (hybrid explorer);
  entries carry a `protected` flag for special folders (system/cloud/junction)
- `GET /api/cleanup/catalog` — cleanup catalog (builtin + custom) for the UI
- `GET /api/tier/preview` · `POST /api/tier/apply`
- `POST /api/backup/run` · `GET /api/backup/status` · `POST /api/backup/restore`
- `GET /api/journals`
- `WS /api/events` — live scan/tiering/backup progress, `index_updated` (watcher
  increment) and `scan_failed` (terminal event on scan errors)

Drive enumeration is cached (30s TTL) and warmed up at startup — on Windows it
shells out to PowerShell (`Get-Disk`/`Get-PhysicalDisk`), which takes seconds, so
`/api/browse` and friends never pay that cost per request. `PUT /api/config`
restarts the real-time watcher with the new `watch.paths` (no daemon restart
needed).

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

# Extra paths the app must never move/delete (built-ins — Windows/system folders,
# cloud-sync folders, junctions — are always protected; see `protected.rs`).
protected_paths = ["E:\\irreplaceable"]

[[rules]]
name = "inactive-projects"
match_glob = "**/*"
inactive_days = 30
from_tier = "fast"
to_tier = "slow"
cleanup_deps = true
junction = true

[backup]
connector = "s3"            # "local" | "s3" | "google-drive"
paths = ["C:\\dev"]
schedule_secs = 0           # 0 = manual only
delete_remote = false
encrypt = true              # encrypt files before upload (ChaCha20-Poly1305)

[backup.options]            # connector-specific:
bucket = "my-backups"       #   s3: bucket, region, endpoint (MinIO/R2/B2);
region = "us-east-1"        #   credentials from AWS_ACCESS_KEY_ID/SECRET or here
#   local: target_root = "D:\\Backup"
#   google-drive: credentials_path = "C:\\secrets\\client_secret.json"

[backup.encryption]
passphrase_env = "OPT_DRIVE_PASSPHRASE"   # passphrase NEVER stored in config
```

## Status

- [x] **Phase 1 — tiering engine (MVP)**: drive detection, indexing, rules,
  move+junction, dependency cleanup, compression, CLI, daemon (REST+WS), Electron UI.
- [x] **Extensible cleanup catalog**: major languages + OS/editor/log junk,
  configurable and expandable via `config.toml`.
- [x] **Real-time indexing**: `notify` file watcher applies incremental changes to
  the index (create/modify/remove) without a full re-scan — the explorer and
  tiering stay always up to date.
- [x] **Protected paths**: system folders (`C:\Windows`, `Program Files`,
  `$RECYCLE.BIN`, …), system files (`pagefile.sys`, …), cloud-sync folders
  (`Google Drive`, `OneDrive`, …) and junctions/reparse points are never indexed
  (system), moved or deleted (all); extra paths via `protected_paths`. The
  explorer marks them with 🔒.
- [x] **Phase 2 — backup / multi-connector**: incremental sync with SHA-256
  manifest (`backup_state`) via `BackupProvider` — local mirror, S3-compatible
  (own SigV4, no AWS SDK), Google Drive (OAuth2 loopback + Drive v3 REST) —
  scheduler job, REST/WS API, CLI and UI. Optional **client-side encryption**
  (`.odenc`, ChaCha20-Poly1305 + Argon2, passphrase via env var) applied both to
  backups and to standalone local files (`opt-drive encrypt/decrypt`).
- [ ] **Phase 3 — polish**: polished UI, more providers (OneDrive, Dropbox), auto-start, guided restore.
