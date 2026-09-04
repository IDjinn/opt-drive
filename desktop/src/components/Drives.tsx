import { memo, useCallback, useEffect, useState } from 'react';
import { api, fmtBytes } from '../api';
import { invalidate, readCache, useCached, writeCache } from '../cache';
import type { Config, DirEntry, Drive, DriveKind, Tier } from '../types';
import Explorer from './Explorer';
import { DrivesSkeleton, RevalidatingBadge } from './Skeleton';

const TIER_COLOR: Record<Tier, string> = {
  fast: '#3fb950',
  slow: '#d29922',
  archive: '#8b949e',
};

const KIND_LABEL: Record<DriveKind, string> = {
  nvme: 'NVMe',
  ssd: 'SSD',
  hdd: 'HDD',
  removable: 'USB',
  unknown: '?',
};

const TIER_LABEL: Record<Tier, string> = {
  fast: 'Rápido',
  slow: 'Médio',
  archive: 'Arquivo',
};

/// Normaliza para comparação case-insensitive com barra final: "C:\Dev" → "c:\dev\".
function normPath(p: string): string {
  return p.replace(/\//g, '\\').toLowerCase().replace(/\\+$/, '') + '\\';
}

/// Label do chip de watch path: parte relativa ao mount, ou "raiz" se for o próprio.
function chipLabel(mount: string, p: string): string {
  const i = p.toLowerCase().indexOf(normPath(mount).slice(0, -1));
  return i === 0 && p.length > mount.length ? p.slice(mount.length) : 'raiz';
}

/// Gauge circular (anel SVG) que preenche conforme `speed_frac` (velocidade relativa),
/// colorido pelo tier. Hover abre tooltip com marca/modelo, interface, RPM e ~read/write.
const SpeedGauge = memo(function SpeedGauge({ d }: { d: Drive }) {
  const color = TIER_COLOR[d.tier];
  const frac = Math.max(0, Math.min(1, d.speed_frac ?? 0));
  const label = KIND_LABEL[d.kind];
  return (
    <div className="gauge-wrap" role="img" aria-label={`Tier ${d.tier}, ${label}`}>
      <svg className="speed-gauge" viewBox="0 0 36 36">
        <circle className="gauge-track" cx="18" cy="18" r="15.91549431" />
        <circle
          className="gauge-arc"
          cx="18"
          cy="18"
          r="15.91549431"
          stroke={color}
          strokeDasharray={`${frac * 100} 100`}
        />
        <text className="gauge-label" x="18" y="20.6">
          {label}
        </text>
      </svg>
      <div className="gauge-tooltip">
        <div className="tt-title">{d.brand || 'Marca desconhecida'}</div>
        {d.model && <div className="tt-line muted">{d.model}</div>}
        <div className="tt-row">
          <span>Interface</span>
          <span>{d.bus || KIND_LABEL[d.kind]}</span>
        </div>
        {d.rotation_rate ? (
          <div className="tt-row">
            <span>Rotação</span>
            <span>{d.rotation_rate} RPM</span>
          </div>
        ) : null}
        <div className="tt-row">
          <span>Leitura</span>
          <span>{d.read_mbps != null ? `~${d.read_mbps} MB/s` : '—'}</span>
        </div>
        <div className="tt-row">
          <span>Escrita</span>
          <span>{d.write_mbps != null ? `~${d.write_mbps} MB/s` : '—'}</span>
        </div>
        <div className="tt-row">
          <span>Total</span>
          <span>{fmtBytes(d.total_bytes)}</span>
        </div>
        <div className="tt-row">
          <span>Livre</span>
          <span>{fmtBytes(d.free_bytes)}</span>
        </div>
        <div className="tt-note">velocidades ~ estimadas por tipo</div>
      </div>
    </div>
  );
});

/// Card de drive isolado (memo) — re-renderiza só quando o drive/estado muda.
/// O rodapé gerencia indexação (watch.paths) e tier override (config.drives).
const DriveCard = memo(function DriveCard({
  d,
  selected,
  onSelect,
  pathsUnder,
  tierOverride,
  busy,
  onToggleIndex,
  onSetTier,
  onRemovePath,
}: {
  d: Drive;
  selected: boolean;
  onSelect: (mount: string) => void;
  pathsUnder: string[];
  tierOverride: Tier | null;
  busy: boolean;
  onToggleIndex: (d: Drive, on: boolean) => void;
  onSetTier: (d: Drive, tier: Tier | null) => void;
  onRemovePath: (d: Drive, path: string) => void;
}) {
  const usedPct = d.total_bytes ? ((d.total_bytes - d.free_bytes) / d.total_bytes) * 100 : 0;
  const freePct = 100 - usedPct;
  return (
    <div
      className={'drive-card' + (selected ? ' selected' : '')}
      onClick={() => onSelect(d.mount)}
    >
      <div className="drive-top">
        <div className="drive-mount">{d.mount}</div>
        <SpeedGauge d={d} />
      </div>
      <div className="drive-model">{d.model || d.label || 'Sem nome'}</div>
      <div className="drive-tags">
        {d.brand && <span className="tag drive-brand">{d.brand}</span>}
        <span className="tag">{d.bus || KIND_LABEL[d.kind]}</span>
        <span className="tag">{d.fs_type}</span>
      </div>
      <div className="bar">
        <div className="bar-fill" style={{ width: `${usedPct}%` }} />
      </div>
      <div className="drive-space">
        <span>{fmtBytes(d.total_bytes - d.free_bytes)} usados</span>
        <span>{fmtBytes(d.free_bytes)} livres</span>
        <span className="free-pct">{freePct.toFixed(0)}% livre</span>
      </div>

      {/* Gerenciamento (não seleciona o card). */}
      <div className="drive-manage" onClick={(e) => e.stopPropagation()}>
        <label className="drive-index-toggle" title="inclui este drive na indexação (watch.paths)">
          <input
            type="checkbox"
            checked={pathsUnder.length > 0}
            disabled={busy}
            onChange={(ev) => onToggleIndex(d, ev.target.checked)}
          />
          Indexar
        </label>
        <label className="drive-tier-field" title="tier usado pelas regras de tiering">
          Tier
          <select
            value={tierOverride ?? ''}
            disabled={busy}
            onChange={(ev) =>
              onSetTier(d, (ev.target.value || null) as Tier | null)
            }
          >
            <option value="">{`Auto (${TIER_LABEL[d.tier]})`}</option>
            <option value="fast">Rápido</option>
            <option value="slow">Médio</option>
            <option value="archive">Arquivo</option>
          </select>
        </label>
      </div>
      {pathsUnder.length > 0 && (
        <div className="drive-paths" onClick={(e) => e.stopPropagation()}>
          {pathsUnder.map((p) => (
            <span className="drive-path-chip" key={p} title={p}>
              {chipLabel(d.mount, p)}
              <button
                className="chip-x"
                disabled={busy}
                title={`remover ${p} da indexação`}
                onClick={() => onRemovePath(d, p)}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
});

export default function Drives({ scanning = false }: { scanning?: boolean }) {
  // Drives raramente mudam → TTL longo (5 min). Render-first: pinta do cache na hora.
  const drivesQ = useCached<Drive[]>('drives', () => api.drives(), {
    ttlMs: 5 * 60_000,
  });
  const { data: drives, error, loading, revalidating, refresh } = drivesQ;
  // Config compartilhada com a aba Regras (mesma cache key) — watch.paths e tier
  // overrides editados aqui aparecem lá e vice-versa.
  const configQ = useCached<Config>('config', () => api.config(), { ttlMs: 30_000 });
  const cfg = configQ.data;

  const [sel, setSel] = useState<string | null>(null);
  const [busyMount, setBusyMount] = useState<string | null>(null);
  const [manageErr, setManageErr] = useState<string | null>(null);

  // Raiz de cada drive sempre em cache: o primeiro clique num card pinta o
  // Explorer na hora, sem skeleton. Falhas silenciosas (daemon offline) — o
  // Explorer refaz o fetch normal ao navegar.
  useEffect(() => {
    if (!drives) return;
    for (const d of drives) {
      const key = 'browse:' + d.mount;
      if (readCache<DirEntry[]>(key) === undefined) {
        api
          .browse(d.mount)
          .then((entries) => writeCache(key, entries))
          .catch(() => {});
      }
    }
  }, [drives]);

  // Salva a config mutada e propaga (cache de config + re-enumera drives, pois
  // tier override muda a classificação).
  const saveCfg = useCallback(
    async (next: Config, mount: string) => {
      setBusyMount(mount);
      setManageErr(null);
      try {
        await api.saveConfig(next);
        configQ.setData(next);
        invalidate('drives');
        refresh();
      } catch (e) {
        setManageErr(e instanceof Error ? e.message : String(e));
      } finally {
        setBusyMount(null);
      }
    },
    [configQ, refresh],
  );

  const onToggleIndex = useCallback(
    (d: Drive, on: boolean) => {
      if (!cfg) return;
      const m = normPath(d.mount);
      const paths = on
        ? [...cfg.watch.paths, d.mount]
        : cfg.watch.paths.filter((p) => !normPath(p).startsWith(m));
      void saveCfg({ ...cfg, watch: { ...cfg.watch, paths } }, d.mount);
    },
    [cfg, saveCfg],
  );

  const onSetTier = useCallback(
    (d: Drive, tier: Tier | null) => {
      if (!cfg) return;
      const m = normPath(d.mount);
      const overrides = cfg.drives.filter((x) => normPath(x.path) !== m);
      const drives = tier ? [...overrides, { path: d.mount, tier }] : overrides;
      void saveCfg({ ...cfg, drives }, d.mount);
    },
    [cfg, saveCfg],
  );

  const onRemovePath = useCallback(
    (d: Drive, path: string) => {
      if (!cfg) return;
      const paths = cfg.watch.paths.filter((p) => p !== path);
      void saveCfg({ ...cfg, watch: { ...cfg.watch, paths } }, d.mount);
    },
    [cfg, saveCfg],
  );

  // Sem cache nenhum → skeleton (mantendo o cabeçalho para estabilidade visual).
  if (loading) return <DrivesSkeleton count={3} />;
  if (error) return <div className="error">Falha ao carregar drives: {error}</div>;

  return (
    <div className="screen">
      <header className="screen-head">
        <h1>
          Drives {revalidating && <RevalidatingBadge />}
        </h1>
        <p>Detecção automática de tipo (NVMe / SSD / HDD) e classificação em tiers. Clique para explorar.</p>
        <div className="actions">
          <button onClick={refresh}>↻ Atualizar</button>
        </div>
      </header>

      {manageErr && <div className="error">Falha ao salvar config: {manageErr}</div>}

      <div className="drive-grid">
        {(drives ?? []).map((d) => {
          const m = normPath(d.mount);
          const pathsUnder = (cfg?.watch.paths ?? []).filter((p) => normPath(p).startsWith(m));
          const tierOverride =
            cfg?.drives.find((x) => normPath(x.path) === m)?.tier ?? null;
          return (
            <DriveCard
              key={d.mount}
              d={d}
              selected={sel === d.mount}
              onSelect={setSel}
              pathsUnder={pathsUnder}
              tierOverride={tierOverride}
              busy={busyMount === d.mount}
              onToggleIndex={onToggleIndex}
              onSetTier={onSetTier}
              onRemovePath={onRemovePath}
            />
          );
        })}
      </div>

      {sel && <Explorer root={sel} scanning={scanning} />}
    </div>
  );
}
