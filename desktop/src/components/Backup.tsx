import { useEffect, useState } from 'react';
import { api, fmtBytes, openEvents } from '../api';
import { useCached } from '../cache';
import type { DaemonEvent, SyncReport } from '../types';

const CONNECTORS: { id: string; label: string; hint: string }[] = [
  { id: 'local', label: 'Local (mirror)', hint: 'Espelha para outra pasta/drive — opção target_root' },
  { id: 's3', label: 'S3 / compatível', hint: 'Opções bucket, region, endpoint (MinIO/R2/B2)' },
  { id: 'google-drive', label: 'Google Drive', hint: 'Opção credentials_path (client_secret)' },
];

/** Aba de backup: status do conector, execução manual com progresso WS e
 *  configuração rápida de conector/encriptação (salva via /api/config). */
export default function Backup() {
  const { data: status, error, loading, refresh } = useCached('backup-status', () =>
    api.backupStatus(),
  );
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<{ current: string; frac: number } | null>(null);
  const [result, setResult] = useState<string | null>(null);

  // Progresso ao vivo via WS durante o "Run now".
  useEffect(() => {
    if (!running) return;
    return openEvents((raw) => {
      const e = raw as DaemonEvent;
      if (e.type === 'backup_progress') setProgress({ current: e.current, frac: e.frac });
      if (e.type === 'backup_done')
        setResult(
          `${e.report.uploaded} enviados, ${e.report.skipped} intactos, ${fmtBytes(
            e.report.bytes_transferred,
          )} transferidos, ${e.report.failed} falhas.`,
        );
    });
  }, [running]);

  const run = async () => {
    setRunning(true);
    setResult(null);
    setProgress(null);
    try {
      const report: SyncReport = await api.backupRun();
      setResult(
        `${report.uploaded} enviados, ${report.skipped} intactos, ${fmtBytes(
          report.bytes_transferred,
        )} transferidos, ${report.failed} falhas.`,
      );
      refresh();
    } catch (e) {
      setResult(`Erro: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setRunning(false);
      setProgress(null);
    }
  };

  return (
    <div className="screen">
      <header className="screen-head">
        <h1>Backup</h1>
        <p>
          Sync incremental para conectores locais ou remotos. Arquivos podem ser{' '}
          <strong>encriptados</strong> (ChaCha20-Poly1305) antes de sair da máquina.
        </p>
        <div className="actions">
          <button onClick={refresh}>↻ Atualizar</button>
          <button className="primary" disabled={running || !status?.enabled} onClick={run}>
            {running ? 'Sincronizando…' : '▶ Executar backup'}
          </button>
        </div>
      </header>

      {result && <div className="callout">{result}</div>}
      {error && <div className="error">{error}</div>}

      {loading && <p className="muted">carregando…</p>}

      {!loading && status && !status.enabled && (
        <div className="empty">
          <p>
            <strong>Backup não configurado.</strong>
          </p>
          <p className="muted">
            Escolha um conector abaixo ou edite a seção <code>[backup]</code> no{' '}
            <code>config.toml</code>. Exemplo:
          </p>
          <pre className="muted">
            {`[backup]
connector = "local"
paths = ["C:\\\\dev"]

[backup.options]
target_root = "D:\\\\Backup"`}
          </pre>
        </div>
      )}

      {!loading && status?.enabled && (
        <>
          {running && progress && (
            <div className="plan-summary">
              <span>{(progress.frac * 100).toFixed(0)}%</span>
              <span className="muted">{progress.current}</span>
            </div>
          )}
          <div className="action-list">
            <div className="action-card">
              <div className="action-kind">
                {CONNECTORS.find((c) => c.id === status.connector)?.label ?? status.connector}
              </div>
              <div className="action-meta">
                <span>{status.entries} itens no manifest</span>
                <span>{status.encrypt ? '🔒 encriptado' : 'sem encriptação'}</span>
                <span>delete_remoto={String(status.delete_remote)}</span>
                {status.schedule_secs > 0 && <span>agenda: {status.schedule_secs}s</span>}
              </div>
              {status.paths.map((p) => (
                <div className="action-src" key={p}>
                  {p}
                </div>
              ))}
            </div>
          </div>
        </>
      )}

      <section className="muted" style={{ marginTop: '2rem' }}>
        <h2 style={{ fontSize: '1rem' }}>Conectores disponíveis</h2>
        <ul>
          {CONNECTORS.map((c) => (
            <li key={c.id}>
              <strong>{c.label}</strong> — {c.hint}
            </li>
          ))}
        </ul>
        <p>
          A encriptação lê a passphrase da variável de ambiente{' '}
          <code>OPT_DRIVE_PASSPHRASE</code> (configurável em{' '}
          <code>[backup.encryption]</code>).
        </p>
      </section>
    </div>
  );
}
