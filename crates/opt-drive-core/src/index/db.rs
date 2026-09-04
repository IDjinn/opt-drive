//! Camada de persistência do índice (SQLite via `rusqlite`).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::FileEntry;

/// Wrapper thread-safe sobre a conexão SQLite. O daemon pode compartilhar uma
/// instância entre tarefas concorrentes (index, queries de policy).
pub struct IndexDb {
    conn: Mutex<Connection>,
}

impl IndexDb {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        // Várias conexões escrevem concorrentemente (watcher, scan completo,
        // backup): em WAL só há um writer por vez e, sem `busy_timeout`, quem
        // chega segundo recebe SQLITE_BUSY imediato ("database is locked").
        // 5s cobre com folga os lotes de 2k entradas do scan.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA synchronous=NORMAL; \
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insere/atualiza uma entrada.
    pub fn upsert(&self, e: &FileEntry) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            SQL_UPSERT,
            params![
                e.path,
                e.is_dir as i64,
                e.size as i64,
                e.mtime,
                e.atime,
                e.drive,
                e.project_root,
            ],
        )?;
        Ok(())
    }

    /// Insere/atualiza várias entradas numa transação.
    pub fn upsert_many(&self, entries: &[FileEntry]) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare_cached(SQL_UPSERT)?;
            for e in entries {
                stmt.execute(params![
                    e.path,
                    e.is_dir as i64,
                    e.size as i64,
                    e.mtime,
                    e.atime,
                    e.drive,
                    e.project_root,
                ])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Aplica um lote misto numa única transação: upserts de entradas novas/atualizadas
    /// e remoções (cada remoção apaga o path e todos os descendentes). Usado pela
    /// indexação incremental.
    pub fn apply_mixed(
        &self,
        upserts: &[FileEntry],
        removes: &[String],
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut up = tx.prepare_cached(SQL_UPSERT)?;
            for e in upserts {
                up.execute(params![
                    e.path,
                    e.is_dir as i64,
                    e.size as i64,
                    e.mtime,
                    e.atime,
                    e.drive,
                    e.project_root,
                ])?;
            }
            let mut rm = tx.prepare_cached(SQL_DELETE_TREE)?;
            for p in removes {
                rm.execute(params![p, subtree_upper_bound(p)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove uma entrada e todos os descendentes indexados. Retorna o nº de linhas
    /// apagadas. Retorna `Ok(0)` se o path não estava indexado.
    pub fn remove_tree(&self, path: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(SQL_DELETE_TREE, params![path, subtree_upper_bound(path)])?;
        Ok(n)
    }

    /// Marca o horário desta indexação (usado p/ "stale" detection).
    pub fn touch_indexed(&self) -> anyhow::Result<()> {
        self.set_meta_u64("last_indexed", unix_now() as u64)
    }

    pub fn last_indexed(&self) -> Option<i64> {
        self.get_meta_u64("last_indexed").map(|v| v as i64)
    }

    /// Estimativa do total de entradas (gravada ao fim de cada scan completo) — usada
    /// para calcular a % e o ETA da próxima varredura.
    pub fn set_scan_total(&self, total: usize) -> anyhow::Result<()> {
        self.set_meta_u64("scan_total_estimate", total as u64)
    }

    pub fn scan_total_estimate(&self) -> Option<usize> {
        self.get_meta_u64("scan_total_estimate").map(|v| v as usize)
    }

    /// Grava um valor inteiro numa chave da tabela `meta`.
    fn set_meta_u64(&self, key: &str, v: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta(k,v) VALUES(?1, ?2)",
            params![key, v as i64],
        )?;
        Ok(())
    }

    /// Lê um valor inteiro de uma chave da tabela `meta` (se existir).
    fn get_meta_u64(&self, key: &str) -> Option<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT v FROM meta WHERE k=?1",
            params![key],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .map(|v| v as u64)
    }

    /// Total de entradas indexadas.
    pub fn count(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    /// Carrega todas as entradas indexadas (para alimentar a policy engine).
    pub fn list_all(&self) -> Vec<FileEntry> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT path, is_dir, size, mtime, atime, drive, project_root FROM files",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], row_to_entry)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect()
    }

    /// Lista entradas por drive (ponto de montagem).
    pub fn list_by_drive(&self, drive: &str) -> Vec<FileEntry> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT path, is_dir, size, mtime, atime, drive, project_root \
                 FROM files WHERE drive = ?1 ORDER BY mtime ASC",
            )
            .unwrap();
        stmt.query_map(params![drive], row_to_entry)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect()
    }

    /// Lista os diretórios de projeto conhecidos (`project_root` distintos).
    pub fn list_projects(&self) -> Vec<FileEntry> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT path, is_dir, size, mtime, atime, drive, project_root \
                 FROM files WHERE is_dir = 1 AND project_root IS NOT NULL \
                 GROUP BY project_root ORDER BY mtime ASC",
            )
            .unwrap();
        stmt.query_map([], row_to_entry)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect()
    }

    /// Insere/atualiza o estado de backup de um arquivo (pós-upload).
    pub fn upsert_backup_entry(&self, e: &BackupEntry) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO backup_state(path, sha256, size, mtime, remote_id, encrypted, synced_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7) \
             ON CONFLICT(path) DO UPDATE SET \
             sha256=excluded.sha256, size=excluded.size, mtime=excluded.mtime, \
             remote_id=excluded.remote_id, encrypted=excluded.encrypted, \
             synced_at=excluded.synced_at",
            params![
                e.path,
                e.sha256,
                e.size as i64,
                e.mtime,
                e.remote_id,
                e.encrypted as i64,
                e.synced_at
            ],
        )?;
        Ok(())
    }

    /// Recupera o último estado de backup de um arquivo (se já foi enviado).
    pub fn get_backup_entry(&self, path: &str) -> Option<BackupEntry> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT path, sha256, size, mtime, remote_id, encrypted, synced_at \
             FROM backup_state WHERE path = ?1",
            params![path],
            row_to_backup_entry,
        )
        .ok()
    }

    /// Total de itens no manifest de backup.
    pub fn backup_state_count(&self) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM backup_state", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Soma o tamanho de todos os descendentes indexados de `path` (inclusive o próprio
    /// dir, se houver linha). Retorna `None` quando nenhuma entrada casa — sinal de que
    /// o diretório não foi indexado (quem chamou deve calcular ao vivo).
    ///
    /// Usa um **range scan na primary key** (`path >= ? AND path < ?`) em vez de
    /// `LIKE`: o índice de `path` tem collation BINARY, então o `LIKE`
    /// (case-insensitive) nunca o aproveita e cada consulta varria a tabela INTEIRA —
    /// num índice de milhões de entradas, listar a raiz de um drive (dezenas de
    /// pastas × scan completo) estourava o timeout de 30s da UI. O range toca só as
    /// linhas da subárvore. Contrapartida: a comparação é case-sensitive — se o caso
    /// de uma pasta mudou desde o último scan, o tamanho fica "—" até re-indexar (o
    /// watcher corrige sozinho num rename).
    pub fn dir_size(&self, path: &str) -> Option<u64> {
        let conn = self.conn.lock().unwrap();
        let hi = subtree_upper_bound(path);
        let total: Option<i64> = conn
            .query_row(
                "SELECT SUM(size) FROM files WHERE path >= ?1 AND path < ?2",
                params![path, hi],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten();
        total.map(|n| n.max(0) as u64)
    }
}

/// Estado de backup de um arquivo (linha da tabela `backup_state`). Serve de
/// manifest incremental: se `sha256`/`mtime` batem, o arquivo não é re-enviado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub path: String,
    /// SHA-256 do conteúdo **efetivamente enviado** (após encriptação, se houver,
    /// usa-se o hash do plaintext para detecção de mudança).
    pub sha256: String,
    pub size: u64,
    pub mtime: i64,
    /// Identificador no destino (chave S3, file id do Drive, caminho no mirror).
    pub remote_id: String,
    pub encrypted: bool,
    pub synced_at: i64,
}

fn row_to_backup_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackupEntry> {
    Ok(BackupEntry {
        path: row.get(0)?,
        sha256: row.get(1)?,
        size: row.get::<_, i64>(2)?.max(0) as u64,
        mtime: row.get(3)?,
        remote_id: row.get(4)?,
        encrypted: row.get::<_, i64>(5)? != 0,
        synced_at: row.get(6)?,
    })
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileEntry> {    Ok(FileEntry {
        path: row.get(0)?,
        is_dir: row.get::<_, i64>(1)? != 0,
        size: row.get::<_, i64>(2)? as u64,
        mtime: row.get(3)?,
        atime: row.get(4)?,
        drive: row.get(5)?,
        project_root: row.get(6)?,
    })
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Limite superior (exclusivo) do range que casa `path` e todos os descendentes:
/// `path` + U+10FFFF (maior codepoint UTF-8). Na comparação byte a byte (BINARY)
/// do índice de `path`, toda linha `path\...` é menor que isso — permite range
/// scan indexado em vez do `LIKE`, que varria a tabela inteira.
fn subtree_upper_bound(path: &str) -> String {
    format!("{path}\u{10FFFF}")
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    is_dir INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    atime INTEGER NOT NULL,
    drive TEXT NOT NULL,
    project_root TEXT,
    last_indexed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_files_drive ON files(drive);
CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);
CREATE INDEX IF NOT EXISTS idx_files_project ON files(project_root);
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v INTEGER);
CREATE TABLE IF NOT EXISTS backup_state (
    path TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    remote_id TEXT NOT NULL,
    encrypted INTEGER NOT NULL DEFAULT 0,
    synced_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_backup_synced ON backup_state(synced_at);
"#;

const SQL_UPSERT: &str = r#"
INSERT INTO files (path, is_dir, size, mtime, atime, drive, project_root, last_indexed)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%s','now'))
ON CONFLICT(path) DO UPDATE SET
    is_dir=excluded.is_dir,
    size=excluded.size,
    mtime=excluded.mtime,
    atime=excluded.atime,
    drive=excluded.drive,
    project_root=excluded.project_root,
    last_indexed=excluded.last_indexed
"#;

/// Apaga um path e seus descendentes. `?2` é o limite superior do range
/// (construído por [`subtree_upper_bound`]) — range scan indexado na PK.
const SQL_DELETE_TREE: &str = r#"DELETE FROM files WHERE path >= ?1 AND path < ?2"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn file(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            is_dir: false,
            size,
            mtime: 0,
            atime: 0,
            drive: "C:\\".into(),
            project_root: None,
        }
    }

    #[test]
    fn dir_size_aggregates_descendants() {
        let tmp = tempdir().unwrap();
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        db.upsert_many(&[
            file("C:\\dev\\proj\\a.txt", 100),
            file("C:\\dev\\proj\\sub\\b.txt", 250),
            file("C:\\outro\\c.txt", 999),
        ])
        .unwrap();

        assert_eq!(db.dir_size("C:\\dev\\proj"), Some(350));
        assert_eq!(db.dir_size("C:\\dev"), Some(350));
        // Não indexado → None.
        assert_eq!(db.dir_size("C:\\naoexiste"), None);
    }

    #[test]
    fn dir_size_escapes_underscore() {
        let tmp = tempdir().unwrap();
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        db.upsert_many(&[
            file("C:\\my_proj\\a.txt", 10),
            file("C:\\myXproj\\b.txt", 9999), // não deve entrar no soma
        ])
        .unwrap();
        assert_eq!(db.dir_size("C:\\my_proj"), Some(10));
    }

    #[test]
    fn busy_timeout_espera_writer_concorrente() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("idx.db");
        let db = IndexDb::open(&path).unwrap();

        // Outra conexão segura o write lock (BEGIN IMMEDIATE) por 300ms.
        let holder = std::thread::spawn(move || {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE;").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = conn.execute_batch("COMMIT;");
        });
        std::thread::sleep(std::time::Duration::from_millis(50)); // lock já tomado

        // Sem busy_timeout isto falharia na hora com "database is locked".
        db.upsert(&file("C:\\a.txt", 1)).unwrap();
        holder.join().unwrap();
        assert_eq!(db.count(), 1);
    }

    /// Bench manual do `dir_size` (range scan na PK):
    /// `cargo test -p opt-drive-core --release bench_dir_size -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_dir_size() {
        let tmp = tempdir().unwrap();
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        // 500k entradas: 50 pastas × 10k arquivos (lotes de 2k, como o scan).
        let mut all = Vec::new();
        for d in 0..50 {
            for f in 0..10_000 {
                all.push(file(&format!("C:\\pastas\\p{d:02}\\f{f}.txt"), 10));
            }
        }
        for chunk in all.chunks(2000) {
            db.upsert_many(chunk).unwrap();
        }

        let t = std::time::Instant::now();
        assert_eq!(db.dir_size("C:\\pastas\\p07"), Some(100_000));
        println!("dir_size de UMA subpasta (10k/500k linhas): {:?}", t.elapsed());

        let t = std::time::Instant::now();
        assert_eq!(db.dir_size("C:\\pastas"), Some(5_000_000));
        println!("dir_size da RAIZ (todas as 500k linhas):    {:?}", t.elapsed());

        assert_eq!(db.dir_size("C:\\naoexiste"), None);
    }

    #[test]
    fn backup_state_upsert_and_get() {
        let tmp = tempdir().unwrap();
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        assert!(db.get_backup_entry("C:\\dev\\a.txt").is_none());

        db.upsert_backup_entry(&BackupEntry {
            path: "C:\\dev\\a.txt".into(),
            sha256: "abc".into(),
            size: 10,
            mtime: 123,
            remote_id: "s3://bucket/dev/a.txt".into(),
            encrypted: true,
            synced_at: 1,
        })
        .unwrap();
        // Upsert atualiza o mesmo path em vez de duplicar.
        db.upsert_backup_entry(&BackupEntry {
            path: "C:\\dev\\a.txt".into(),
            sha256: "def".into(),
            size: 20,
            mtime: 456,
            remote_id: "s3://bucket/dev/a.txt".into(),
            encrypted: true,
            synced_at: 2,
        })
        .unwrap();

        let e = db.get_backup_entry("C:\\dev\\a.txt").unwrap();
        assert_eq!(e.sha256, "def");
        assert_eq!(e.size, 20);
        assert!(e.encrypted);
    }
}
