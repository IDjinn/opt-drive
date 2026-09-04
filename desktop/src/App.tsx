import { useEffect, useRef, useState } from 'react';
import { api, apiBase, fmtBytes, openEvents } from './api';
import { useCached } from './cache';
import type { DaemonEvent, IndexStatus, ScanProgress } from './types';
import Drives from './components/Drives';
import Index from './components/Index';
import Tiering from './components/Tiering';
import Rules from './components/Rules';
import Backup from './components/Backup';

type Tab = 'drives' | 'index' | 'tiering' | 'rules' | 'backup';

/** Resumo curto da varredura p/ o sidebar (ex.: "Indexando… 73% · 12k entradas · ETA 2m"). */
function scanSummary(p: ScanProgress): string {
  const pct =
    p.total_estimate && p.total_estimate > 0
      ? Math.min(100, Math.round((p.indexed / p.total_estimate) * 100))
      : null;
  const s = p.elapsed_ms / 1000;
  const rate = s > 0 ? p.indexed / s : 0;
  const rem = p.total_estimate ? Math.max(p.total_estimate - p.indexed, 0) : null;
  const etaSec = rem != null && rate > 0 ? Math.round(rem / rate) : null;
  const eta =
    etaSec == null
      ? ''
      : etaSec < 60
        ? `${etaSec}s`
        : `${Math.floor(etaSec / 60)}m`;
  const rateStr = rate >= 1000 ? `${(rate / 1000).toFixed(1)}k` : `${Math.round(rate)}`;
  return `Indexando… ${pct != null ? pct + '% · ' : ''}${p.indexed.toLocaleString()} entradas · ${rateStr}/s${
    eta ? ' · ETA ' + eta : ''
  }`;
}

export default function App() {
  const [tab, setTab] = useState<Tab>('drives');
  // Abas já visitadas ficam montadas (escondidas quando inativas) — preserva estado,
  // scroll e cache ao trocar de aba, evitando refetch.
  const [mounted, setMounted] = useState<Set<Tab>>(() => new Set(['drives']));
  const [liveActivity, setLiveActivity] = useState<string | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  // Estado de varredura (lifted) — alimentado pelos eventos WS, consumido pela aba Index.
  const [scanning, setScanning] = useState(false);
  const [scan, setScan] = useState<ScanProgress | null>(null);
  // Throttle de refresh do contador do índice: durante um `npm install`/`cargo build`
  // chegam dezenas de eventos `index_updated`; sem isso bombardearíamos /api/index/status.
  const lastStatusRefresh = useRef(0);

  const base = apiBase();

  // Status do índice via cache — aparece do cache antes do fetch.
  const indexQ = useCached<IndexStatus>('index-status', () => api.indexStatus(), {
    ttlMs: 15_000,
  });
  const status = indexQ.data ?? null;

  useEffect(() => {
    if (!mounted.has(tab)) setMounted(new Set(mounted).add(tab));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  useEffect(() => {
    const close = openEvents((e) => {
      const ev = e as DaemonEvent;
      switch (ev.type) {
        case 'scan_started':
          setScanning(true);
          // Semeia a barra com a estimativa do último scan (o % aparece imediatamente).
          setScan({
            indexed: 0,
            current_dir: null,
            total_estimate: ev.total_estimate,
            elapsed_ms: 0,
            bytes: 0,
            errors: 0,
          });
          setLiveActivity('Indexando…');
          break;
        case 'scan_progress':
          setScanning(true);
          setScan(ev);
          setLiveActivity(scanSummary(ev));
          break;
        case 'scan_done':
          setScanning(false);
          setScan(null);
          setLiveActivity(null);
          indexQ.setData({
            entries: ev.stats.entries_indexed,
            last_indexed: Math.floor(Date.now() / 1000),
          });
          setRefreshKey((k) => k + 1); // só Tiering revalida (deps)
          break;
        case 'scan_canceled':
          setScanning(false);
          setScan(null);
          setLiveActivity(null);
          indexQ.refresh();
          break;
        case 'scan_failed':
          setScanning(false);
          setScan(null);
          setLiveActivity(`Falha na indexação: ${ev.error}`);
          indexQ.refresh();
          break;
        case 'index_updated': {
          // Incremento em tempo real (contador e revalidações no MESMO throttle de
          // 3s). Sem o throttle, cada evento cancelava o fetch do tier/preview em
          // andamento (o useCached descarta a resposta do effect anterior) e a aba
          // Tiering ficava em skeleton eterno durante atividade do watcher.
          if (Date.now() - lastStatusRefresh.current > 3000) {
            lastStatusRefresh.current = Date.now();
            indexQ.refresh();
            setRefreshKey((k) => k + 1);
          }
          break;
        }
        case 'tier_progress':
          setLiveActivity(`${(ev.frac * 100).toFixed(0)}% — ${ev.desc}`);
          break;
        case 'tier_done':
          setLiveActivity(null);
          setRefreshKey((k) => k + 1);
          break;
        case 'backup_started':
          setLiveActivity(`Backup [${ev.connector}] iniciado…`);
          break;
        case 'backup_progress':
          setLiveActivity(
            ev.current ? `Backup ${(ev.frac * 100).toFixed(0)}% — ${ev.current}` : 'Backup…',
          );
          break;
        case 'backup_done':
          setLiveActivity(null);
          break;
        default:
          break;
      }
    });
    return close;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <span className="logo">⬡</span>
          <div>
            <div className="brand-name">Opt-Drive</div>
            <div className="brand-sub">tiering & backup</div>
          </div>
        </div>
        <nav>
          <button className={tab === 'drives' ? 'active' : ''} onClick={() => setTab('drives')}>
            💽 Drives
          </button>
          <button className={tab === 'index' ? 'active' : ''} onClick={() => setTab('index')}>
            🗂️ Indexação
          </button>
          <button className={tab === 'tiering' ? 'active' : ''} onClick={() => setTab('tiering')}>
            🔀 Tiering
          </button>
          <button className={tab === 'rules' ? 'active' : ''} onClick={() => setTab('rules')}>
            ⚙️ Regras
          </button>
          <button className={tab === 'backup' ? 'active' : ''} onClick={() => setTab('backup')}>
            ☁️ Backup
          </button>
        </nav>
        <div className="status-card">
          <div className="status-row">
            <span>Índice</span>
            <strong>{status ? status.entries.toLocaleString() : '—'}</strong>
          </div>
          <div className="status-row">
            <span>Última varredura</span>
            <strong>
              {status?.last_indexed
                ? new Date(status.last_indexed * 1000).toLocaleString()
                : '—'}
            </strong>
          </div>
          {liveActivity && <div className="live">{liveActivity}</div>}
          <button
            className="scan-btn"
            disabled={scanning}
            onClick={() => {
              setTab('index');
              api.indexRun().catch((e) => setLiveActivity(`Erro ao indexar: ${e.message ?? e}`));
            }}
          >
            {scanning ? 'Indexando…' : '↻ Re-indexar'}
          </button>
        </div>
        {!base && (
          <div className="warn">daemon offline — sem conexão com a API local.</div>
        )}
      </aside>

      <main className="content">
        {[...mounted].map((t) => (
          <div key={t} className={tab === t ? 'tab-panel' : 'tab-panel hidden'}>
            {t === 'drives' && <Drives scanning={scanning} />}
            {t === 'index' && <Index scanning={scanning} scan={scan} refreshKey={refreshKey} />}
            {t === 'tiering' && <Tiering refreshKey={refreshKey} />}
            {t === 'rules' && <Rules />}
            {t === 'backup' && <Backup />}
          </div>
        ))}
      </main>
    </div>
  );
}

// reexporta para uso em telas
export { fmtBytes };
