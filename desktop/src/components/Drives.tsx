import { memo, useState } from 'react';
import { api, fmtBytes } from '../api';
import { useCached } from '../cache';
import type { Drive, DriveKind, Tier } from '../types';
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

/// Card de drive isolado (memo) — re-renderiza só quando o drive muda ou ganha/perde seleção.
const DriveCard = memo(function DriveCard({
  d,
  selected,
  onSelect,
}: {
  d: Drive;
  selected: boolean;
  onSelect: (mount: string) => void;
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
    </div>
  );
});

export default function Drives() {
  // Drives raramente mudam → TTL longo (5 min). Render-first: pinta do cache na hora.
  const { data: drives, error, loading, revalidating, refresh } = useCached<Drive[]>(
    'drives',
    () => api.drives(),
    { ttlMs: 5 * 60_000 },
  );
  const [sel, setSel] = useState<string | null>(null);

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

      <div className="drive-grid">
        {(drives ?? []).map((d) => (
          <DriveCard key={d.mount} d={d} selected={sel === d.mount} onSelect={setSel} />
        ))}
      </div>

      {sel && <Explorer root={sel} />}
    </div>
  );
}
