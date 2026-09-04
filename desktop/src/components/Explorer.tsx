import { useEffect, useState } from 'react';
import { api, fmtBytes } from '../api';
import { useCached } from '../cache';
import type { DirEntry } from '../types';
import { ExplorerSkeleton } from './Skeleton';

/// Concatena `parent` + `name` respeitando o separador do Windows.
function joinPath(parent: string, name: string): string {
  if (parent.endsWith('\\') || parent.endsWith('/')) return parent + name;
  return parent + '\\' + name;
}

/// Quebra um path Windows ("C:\\dev\\foo") em segmentos clicáveis para o breadcrumb.
function segments(path: string): { label: string; path: string }[] {
  const norm = path.replace(/\//g, '\\');
  const rootMatch = norm.match(/^([A-Za-z]:\\)/);
  if (!rootMatch) return [{ label: norm, path: norm }];
  const root = rootMatch[1];
  const parts: { label: string; path: string }[] = [{ label: root, path: root }];
  const rest = norm.slice(root.length);
  if (rest) {
    let acc = root;
    for (const seg of rest.split('\\').filter(Boolean)) {
      acc = joinPath(acc, seg);
      parts.push({ label: seg, path: acc });
    }
  }
  return parts;
}

function fmtDate(unix: number): string {
  if (!unix) return '—';
  const d = new Date(unix * 1000);
  return d.toLocaleDateString('pt-BR');
}

export default function Explorer({ root, scanning = false }: { root: string; scanning?: boolean }) {
  const [path, setPath] = useState(root);
  const [scanErr, setScanErr] = useState<string | null>(null);

  // Trocou o drive selecionado → volta para a raiz dele.
  useEffect(() => {
    setPath(root);
  }, [root]);

  // Segurança client; path não deve escapar da raiz navegável.
  const safe = !path.includes('..');

  // Cache por pasta (60s). Render-first: pasta já visitada aparece na hora.
  const { data: entries, error, loading, revalidating } = useCached<DirEntry[]>(
    'browse:' + path,
    () => api.browse(path),
    { ttlMs: 60_000 },
  );

  const go = (p: string) => setPath(p);
  const crumbs = segments(path);
  // Windows é case-insensitive: comparar em minúsculas evita "C:\DEV" ≠ "c:\dev".
  const atRoot = path.toLowerCase() === root.toLowerCase();

  return (
    <section className="explorer">
      <div className="explorer-head">
        <div className="explorer-title">
          <h2>
            Explorer {revalidating && <span className="revalidating-dot" />}
          </h2>
          {atRoot && (
            <button
              className="link-btn"
              disabled={scanning}
              title={scanning ? 'varredura em andamento' : 'varre os watch paths configurados'}
              onClick={() => {
                setScanErr(null);
                api
                  .indexRun()
                  .catch((e) => setScanErr(String(e.message ?? e)));
              }}
            >
              {scanning ? 'Indexando…' : '⚡ Indexar agora'}
            </button>
          )}
        </div>
        <nav className="breadcrumb">
          {crumbs.map((s, i) => (
            <span className="crumb" key={s.path}>
              <button
                className="crumb-btn"
                onClick={() => go(s.path)}
                disabled={i === crumbs.length - 1}
              >
                {s.label}
              </button>
              {i < crumbs.length - 1 && <span className="crumb-sep">›</span>}
            </span>
          ))}
        </nav>
      </div>

      {atRoot && (
        <div className="callout subtle">
          Para ver o <strong>tamanho das pastas</strong>, indexe este drive: ative{' '}
          <strong>Indexar</strong> no card do drive (ou edite <strong>Regras → Watch paths</strong>)
          e clique em “Indexar agora”. Pastas não indexadas aparecem com “—”. A estrutura de
          arquivos sempre é lista em tempo real.
        </div>
      )}

      {!safe && <div className="error">Caminho inválido.</div>}
      {safe && scanErr && <div className="error">{scanErr}</div>}
      {safe && error && <div className="error">{error}</div>}
      {safe && loading && <ExplorerSkeleton count={7} />}
      {safe && !loading && entries && entries.length === 0 && (
        <div className="empty">Pasta vazia.</div>
      )}
      {safe && !loading && entries && entries.length > 0 && (
        <div className="explorer-list">
          <div className="explorer-row explorer-row-head">
            <span className="ex-name">Nome</span>
            <span className="ex-size">Tamanho</span>
            <span className="ex-mtime">Modificado</span>
          </div>
          {entries.map((e) => (
            <div
              key={e.name}
              className={'explorer-row' + (e.is_dir ? ' is-dir' : '')}
              onClick={e.is_dir ? () => go(joinPath(path, e.name)) : undefined}
            >
              <span className="ex-name">
                <span className="ex-icon">{e.is_dir ? '📁' : '📄'}</span>
                <span className="ex-fname">{e.name}</span>
                {e.protected && (
                  <span
                    className="ex-protected"
                    title="caminho especial (sistema/nuvem/junction) — protegido contra exclusão e movimentação"
                  >
                    🔒
                  </span>
                )}
                {e.is_dir &&
                  (e.indexed ? (
                    <span className="indexed-dot" title="tamanho do índice" />
                  ) : (
                    <span className="unknown-dot" title="não indexado — rode a indexação" />
                  ))}
              </span>
              <span className="ex-size">
                {e.is_dir && !e.indexed ? '—' : fmtBytes(e.size_bytes)}
              </span>
              <span className="ex-mtime">{fmtDate(e.mtime)}</span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
