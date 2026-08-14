// Skeletons (placeholders shimmer) para cada tela.
// Uso: mostrar enquanto `loading` (sem cache). Espelham o layout real pra reduzir CLS.

import type { CSSProperties } from 'react';

interface SkProps {
  w?: number | string;
  h?: number | string;
  r?: number | string;
  className?: string;
  style?: CSSProperties;
}

/// Bloco shimmer genérico (`display` controlado pelo CSS `.sk` para permitir overrides).
export function Skeleton({ w = '100%', h = 12, r = 6, className, style }: SkProps) {
  return (
    <span
      className={'sk' + (className ? ' ' + className : '')}
      style={{ width: w, height: h, borderRadius: r, ...style }}
      aria-hidden
    />
  );
}

/// Botão "ghost" revalidando (sutil), p/ o header de telas com cache quente.
export function RevalidatingBadge() {
  return (
    <span className="revalidating" title="atualizando…">
      <span className="revalidating-dot" />
    </span>
  );
}

/// Grid de cards de drive (cabeçalho + N placeholders de drive-card).
export function DrivesSkeleton({ count = 3 }: { count?: number }) {
  return (
    <div className="screen">
      <header className="screen-head">
        <Skeleton w={120} h={24} />
        <Skeleton w={'min(80%, 520px)'} h={12} style={{ marginTop: 8 }} />
      </header>
      <div className="drive-grid">
        {Array.from({ length: count }).map((_, i) => (
          <div className="drive-card" key={i}>
            <div className="drive-top">
              <Skeleton w={56} h={20} r={6} />
              <Skeleton w={46} h={46} r={'50%'} />
            </div>
            <Skeleton w={'60%'} h={13} style={{ margin: '10px 0' }} />
            <div className="drive-tags">
              <Skeleton w={60} h={18} />
              <Skeleton w={48} h={18} />
              <Skeleton w={40} h={18} />
            </div>
            <Skeleton className="sk-bar" h={8} r={999} style={{ marginTop: 6 }} />
            <div className="drive-space">
              <Skeleton w={90} h={11} />
              <Skeleton w={80} h={11} />
              <Skeleton w={52} h={11} />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/// Linhas do Explorer (cabeçalho da tabela + N linhas).
export function ExplorerSkeleton({ count = 7 }: { count?: number }) {
  return (
    <div className="explorer-list">
      <div className="explorer-row explorer-row-head">
        <span>Nome</span>
        <span>Tamanho</span>
        <span>Modificado</span>
      </div>
      {Array.from({ length: count }).map((_, i) => (
        <div className="explorer-row" key={i}>
          <span className="ex-name" style={{ gap: 8 }}>
            <Skeleton w={15} h={15} />
            <Skeleton w={`${50 + ((i * 13) % 40)}%`} h={12} />
          </span>
          <span className="ex-size">
            <Skeleton w={60} h={11} style={{ marginLeft: 'auto' }} />
          </span>
          <span className="ex-mtime">
            <Skeleton w={70} h={11} style={{ marginLeft: 'auto' }} />
          </span>
        </div>
      ))}
    </div>
  );
}

/// Cabeçalho do Tiering + action cards placeholder.
export function TieringSkeleton({ count = 3 }: { count?: number }) {
  return (
    <>
      <div className="plan-summary">
        <Skeleton w={90} h={13} />
        <Skeleton w={130} h={13} />
      </div>
      <div className="action-list">
        {Array.from({ length: count }).map((_, i) => (
          <div className="action-card" key={i}>
            <Skeleton w={150} h={14} />
            <Skeleton w={'70%'} h={13} style={{ marginTop: 8 }} />
            <Skeleton w={'55%'} h={12} style={{ marginTop: 6 }} />
            <div className="action-meta">
              <Skeleton w={60} h={11} />
              <Skeleton w={70} h={11} />
              <Skeleton w={80} h={11} />
            </div>
          </div>
        ))}
      </div>
    </>
  );
}

/// Seções do Rules (watch paths + accordion + rules).
export function RulesSkeleton() {
  return (
    <>
      <section className="cfg-section">
        <Skeleton w={110} h={15} />
        <Skeleton className="sk-bar" h={64} r={8} style={{ marginTop: 10 }} />
        <Skeleton w={90} h={11} style={{ marginTop: 12 }} />
        <Skeleton className="sk-bar" h={34} r={8} style={{ marginTop: 4 }} />
      </section>
      <section className="cfg-section">
        <Skeleton w={150} h={15} />
        <div className="accordion" style={{ marginTop: 10 }}>
          {Array.from({ length: 3 }).map((_, i) => (
            <div className="accordion-item" key={i}>
              <div className="accordion-head">
                <Skeleton w={160} h={14} />
                <Skeleton w={100} h={14} />
              </div>
            </div>
          ))}
        </div>
      </section>
      <section className="cfg-section">
        <Skeleton w={130} h={15} />
        <div className="rule-card" style={{ marginTop: 10 }}>
          <Skeleton w={'40%'} h={16} />
          <div className="rule-grid" style={{ marginTop: 12 }}>
            {Array.from({ length: 4 }).map((_, i) => (
              <Skeleton key={i} className="sk-bar" h={48} r={8} />
            ))}
          </div>
        </div>
      </section>
    </>
  );
}
