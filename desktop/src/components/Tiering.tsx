import { useState } from 'react';
import { api, fmtBytes } from '../api';
import { useCached } from '../cache';
import type { Plan } from '../types';
import { RevalidatingBadge, TieringSkeleton } from './Skeleton';

export default function Tiering({ refreshKey }: { refreshKey: number }) {
  // Plan depende do índice → revalida quando refreshKey muda (após scan/tier).
  const { data: plan, error, loading, revalidating, refresh } = useCached<Plan>(
    'tier',
    () => api.tierPreview(),
    { ttlMs: 15_000, deps: [refreshKey] },
  );
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const apply = async () => {
    setBusy(true);
    setResult(null);
    try {
      // O daemon retorna o RunReport puro — desestruturar {report} dava TypeError.
      const report = await api.tierApply();
      const journal = report.journal_id ? ` · journal ${report.journal_id}` : '';
      setResult(
        `${report.relocated} movidos, ${report.compressed} comprimidos, ~${fmtBytes(
          report.bytes_cleaned,
        )} limpos, ${report.errors} erros${journal}.`,
      );
      refresh(); // revalida o plano (ações aplicadas)
    } catch (e) {
      setResult(`Erro: ${e instanceof Error ? e.message : e}`);
    } finally {
      setBusy(false);
    }
  };

  const count = plan?.actions.length ?? 0;

  return (
    <div className="screen">
      <header className="screen-head">
        <h1>
          Tiering {revalidating && <RevalidatingBadge />}
        </h1>
        <p>
          Projetos inativos são candidatos a mudar para o drive mais lento. O preview é{' '}
          <strong>dry-run</strong>; aplicar move os dados e cria junction no caminho original.
        </p>
        <div className="actions">
          <button onClick={refresh}>↻ Atualizar preview</button>
          <button
            className="primary"
            disabled={!plan || count === 0 || busy}
            onClick={apply}
          >
            {busy ? 'Aplicando…' : `▶ Aplicar (${count})`}
          </button>
        </div>
      </header>

      {result && <div className="callout">{result}</div>}
      {error && <div className="error">{error}</div>}

      {loading && <TieringSkeleton count={3} />}

      {!loading && plan && count === 0 && (
        <div className="empty">
          <p>
            <strong>Nenhuma ação de tiering no momento.</strong> 🎯
          </p>
          <p className="muted">
            O tiering move/comprime <strong>projetos inteiros</strong> (pastas com{' '}
            <code>.git</code>) inativos há mais dias que o limite da regra. Para gerar ações:
          </p>
          <ol className="muted">
            <li>
              Em <strong>Regras → Watch paths</strong>, adicione as raízes a monitorar (ex.{' '}
              <code>C:\dev</code>).
            </li>
            <li>
              Clique em <strong>↻ Re-indexar</strong> (sidebar) para varrer essas raízes.
            </li>
            <li>
              Ajuste <strong>Regras de tiering</strong> (glob, dias inativo, de/para tier) se precisar.
            </li>
          </ol>
          <p className="muted">
            Quando um projeto ficar inativo pelo prazo definido, ele aparecerá aqui como ação de
            relocate/compress.
          </p>
        </div>
      )}

      {!loading && plan && count > 0 && (
        <>
          <div className="plan-summary">
            <span>{count} ação(ões)</span>
            <span>~{fmtBytes(plan.total_bytes_moved)} a mover</span>
          </div>
          <div className="action-list">
            {plan.actions.map((a, i) => (
              <div className="action-card" key={i}>
                <div className="action-kind">
                  {a.kind === 'relocate' ? '↪ Relocate' : '📦 Compress'}
                  <span className="muted">[{a.rule}]</span>
                </div>
                <div className="action-src">{a.src}</div>
                <div className="action-dst">→ {a.dst}</div>
                {a.kind === 'relocate' && (
                  <div className="action-meta">
                    <span>{fmtBytes(a.size_bytes)}</span>
                    <span>inativo {a.days_inactive}d</span>
                    <span>junction={String(a.junction)}</span>
                    <span>cleanup={String(a.cleanup_deps)}</span>
                  </div>
                )}
                {a.kind === 'compress' && (
                  <div className="action-meta">
                    <span>{fmtBytes(a.size_bytes)}</span>
                    <span>cleanup={String(a.cleanup_deps)}</span>
                  </div>
                )}
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
