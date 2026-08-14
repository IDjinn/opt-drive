import { useEffect, useRef, useState } from 'react';
import { api, fmtBytes } from '../api';
import { useCached, writeCache } from '../cache';
import type { Config, IndexStatus, ScanProgress } from '../types';

interface Props {
  /** Há uma varredura em andamento? (controlado pelo App via eventos WS). */
  scanning: boolean;
  /** Snapshot mais recente do progresso (null se ocioso). */
  scan: ScanProgress | null;
  /** Bump p/ revalidar o status do índice após scan_done/index_updated. */
  refreshKey: number;
}

/** Tiles exibidos durante a varredura. */
function derive(scan: ScanProgress) {
  const elapsedS = scan.elapsed_ms / 1000;
  const rate = elapsedS > 0 ? scan.indexed / elapsedS : 0;
  const est = scan.total_estimate && scan.total_estimate > 0 ? scan.total_estimate : null;
  const frac = est != null ? Math.min(Math.max(scan.indexed / est, 0), 1) : null;
  const pct = frac != null ? Math.round(frac * 100) : null;
  const remaining = est != null ? Math.max(est - scan.indexed, 0) : null;
  const etaMs = remaining != null && rate > 0 ? (remaining / rate) * 1000 : null;
  return { rate, pct, etaMs };
}

function fmtRate(r: number): string {
  if (r >= 1000) return `${(r / 1000).toFixed(1)}k`;
  return `${Math.round(r)}`;
}

function fmtDur(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) {
    const m = Math.floor(s / 60);
    return `${m}m${String(s % 60).padStart(2, '0')}s`;
  }
  const h = Math.floor(s / 3600);
  return `${h}h${Math.floor((s % 3600) / 60)}m`;
}

export default function Index({ scanning, scan, refreshKey }: Props) {
  const indexQ = useCached<IndexStatus>('index-status', () => api.indexStatus(), {
    ttlMs: 15_000,
    deps: [refreshKey],
  });
  const configQ = useCached<Config>('config', () => api.config(), { ttlMs: 30_000 });
  const cfg = configQ.data ?? null;
  const status = indexQ.data ?? null;

  // --- Editor de threads (salva com debounce, sempre fazendo merge contra o config
  //     fresco do servidor p/ não clobberar campos editados em outras abas). ---
  const [threads, setThreads] = useState<number>(cfg?.indexer?.threads ?? 0);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const lastSync = useRef<number | null>(null);

  // Sincroniza o input quando o valor em cache muda (ex.: carregou ou salvou).
  useEffect(() => {
    const v = cfg?.indexer?.threads ?? 0;
    if (lastSync.current !== v) {
      lastSync.current = v;
      setThreads(v);
    }
  }, [cfg?.indexer?.threads]);

  useEffect(() => {
    if (!cfg) return;
    if ((cfg.indexer?.threads ?? 0) === threads) return;
    const t = setTimeout(() => {
      (async () => {
        try {
          const fresh = await api.config();
          // Merge contra o config fresco; preserva campos extras de `indexer` se houver.
          const merged: Config = {
            ...fresh,
            indexer: { ...fresh.indexer, threads },
          };
          await api.saveConfig(merged);
          writeCache('config', merged);
          configQ.setData(merged);
          setSaveErr(null);
        } catch (e) {
          setSaveErr(e instanceof Error ? e.message : String(e));
        }
      })();
    }, 450);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threads]);

  const start = () => {
    api.indexRun().catch((e) => setSaveErr(e instanceof Error ? e.message : String(e)));
  };
  const cancel = () => {
    api.indexCancel().catch(() => {});
  };

  const live = scanning && scan;
  const d = live ? derive(scan) : null;

  return (
    <div className="screen">
      <div className="screen-head">
        <div>
          <h1>🗂️ Indexação</h1>
          <p>
            Varredura paralela dos watch paths. O índice alimenta a detecção de projetos
            inativos e o tiering. É incremental em tempo real — a varredura completa é
            para reconstruir do zero.
          </p>
        </div>
        <div className="actions">
          {scanning ? (
            <button className="danger" onClick={cancel}>
              ■ Cancelar
            </button>
          ) : (
            <button
              className="primary"
              onClick={start}
              disabled={!cfg || (cfg.watch?.paths?.length ?? 0) === 0}
            >
              ▶ Iniciar varredura
            </button>
          )}
        </div>
      </div>

      {live && d ? (
        <div className="scan-progress">
          <div className={`scan-bar${scan.total_estimate ? '' : ' indeterminate'}`}>
            {scan.total_estimate ? (
              <div className="scan-bar-fill" style={{ width: `${d.pct ?? 0}%` }} />
            ) : (
              <div className="scan-bar-pulse" />
            )}
          </div>
          <div className="scan-pct-row">
            <strong>{d.pct != null ? `${d.pct}%` : 'varrendo…'}</strong>
            <span className="muted">
              {fmtRate(d.rate)}/s{d.etaMs != null ? ` · ETA ${fmtDur(d.etaMs)}` : ''}
            </span>
          </div>
          <div className="scan-current" title={scan.current_dir ?? ''}>
            <span className="ex-icon">📁</span>
            <span className="scan-current-path">{scan.current_dir ?? '—'}</span>
          </div>
          <div className="scan-tiles">
            <Tile
              label="Entradas"
              value={`${scan.indexed.toLocaleString()}${
                scan.total_estimate ? ' / ' + scan.total_estimate.toLocaleString() : ''
              }`}
            />
            <Tile label="Velocidade" value={`${fmtRate(d.rate)}/s`} />
            <Tile label="Decorrido" value={fmtDur(scan.elapsed_ms)} />
            <Tile label="ETA" value={d.etaMs != null ? fmtDur(d.etaMs) : '—'} />
            <Tile label="Bytes" value={fmtBytes(scan.bytes)} />
            <Tile label="Erros" value={scan.errors.toLocaleString()} warn={scan.errors > 0} />
          </div>
        </div>
      ) : (
        <div className="scan-tiles">
          <Tile label="Entradas indexadas" value={status ? status.entries.toLocaleString() : '—'} />
          <Tile
            label="Última varredura"
            value={status?.last_indexed ? new Date(status.last_indexed * 1000).toLocaleString() : '—'}
          />
          <Tile label="Watch paths" value={cfg ? (cfg.watch?.paths?.length ?? 0).toLocaleString() : '—'} />
        </div>
      )}

      <div className="cfg-section">
        <h2>Desempenho</h2>
        <label className="field-label" htmlFor="idx-threads">
          Threads de varredura
        </label>
        <div className="scan-threads">
          <input
            id="idx-threads"
            type="number"
            min={0}
            className="cfg-input scan-threads-input"
            value={threads}
            onChange={(e) => {
              const n = Number.parseInt(e.target.value, 10);
              setThreads(Number.isFinite(n) && n >= 0 ? n : 0);
            }}
          />
          <span className="scan-threads-hint muted">
            {threads === 0
              ? 'automático = nº de CPUs'
              : `${threads} thread${threads === 1 ? '' : 's'}`}
          </span>
        </div>
        <div className="scan-note">
          0 = automático (todos os núcleos). Reduzir limita o uso de CPU durante a
          varredura — útil para não competir com tarefas pesadas do sistema.
        </div>
        {saveErr && <div className="error scan-save-err">{saveErr}</div>}
      </div>
    </div>
  );
}

function Tile({ label, value, warn }: { label: string; value: string; warn?: boolean }) {
  return (
    <div className={`scan-tile${warn ? ' warn' : ''}`}>
      <div className="scan-tile-label">{label}</div>
      <div className="scan-tile-value">{value}</div>
    </div>
  );
}
