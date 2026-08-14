import { useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import { useCached } from '../cache';
import type { CleanupTarget, Config } from '../types';
import { RevalidatingBadge, RulesSkeleton } from './Skeleton';

/// Categorias de linguagem → grupo "Linguagens & frameworks".
const LANG_CATS = new Set([
  'javascript',
  'typescript',
  'rust',
  'python',
  'java',
  'cpp',
  'csharp',
  'swift',
  'elixir',
  'haskell',
  'dart',
  'lua',
  'r',
  'scala',
  'php',
]);

function groupOf(cat: string): string {
  if (LANG_CATS.has(cat)) return 'Linguagens & frameworks';
  if (cat === 'os' || cat === 'editor') return 'Sistema & editores';
  if (cat === 'logs' || cat === 'cache') return 'Logs & caches';
  return 'Outros';
}

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v));
}

export default function Rules() {
  // Config pelo cache (30s). Catálogo é praticamente estático (10 min).
  const configQ = useCached<Config>('config', () => api.config(), { ttlMs: 30_000 });
  const catalogQ = useCached<CleanupTarget[]>('catalog', () => api.cleanupCatalog(), {
    ttlMs: 10 * 60_000,
  });
  const catalog = catalogQ.data ?? [];

  const [cfg, setCfg] = useState<Config | null>(null); // último valor salvo/carregado
  const [draft, setDraft] = useState<Config | null>(null); // edição local
  const [saved, setSaved] = useState(false);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const [openGroups, setOpenGroups] = useState<Record<string, boolean>>({
    'Linguagens & frameworks': true,
  });

  // Sincroniza cfg com o cache; draft só na 1ª carga (preserva edições do usuário).
  useEffect(() => {
    const c = configQ.data;
    if (!c) return;
    setCfg(c);
    setDraft((prev) => prev ?? clone(c));
  }, [configQ.data]);

  // Catálogo embutido agrupado (exclui custom — esses ficam no textarea avançado).
  const groups = useMemo(() => {
    const map = new Map<string, CleanupTarget[]>();
    for (const t of catalog) {
      if (t.category === 'custom') continue;
      const g = groupOf(t.category);
      if (!map.has(g)) map.set(g, []);
      map.get(g)!.push(t);
    }
    // ordenação estável dos grupos
    const order = [
      'Linguagens & frameworks',
      'Sistema & editores',
      'Logs & caches',
      'Outros',
    ];
    return order.filter((g) => map.has(g)).map((g) => ({ group: g, targets: map.get(g)! }));
  }, [catalog]);

  const revert = () => {
    if (cfg) setDraft(clone(cfg));
  };

  const save = async () => {
    if (!draft) return;
    setSaveErr(null);
    setSaved(false);
    try {
      await api.saveConfig(draft);
      configQ.setData(draft); // atualiza cache + estado otimista (sem refetch)
      setCfg(draft);
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    } catch (e) {
      setSaveErr(String(e));
    }
  };

  const dirty = !!cfg && !!draft && JSON.stringify(cfg) !== JSON.stringify(draft);

  const update = (patch: Partial<Config>) => setDraft((d) => (d ? { ...d, ...patch } : d));

  // --- toggles do catálogo de cleanup (blacklist em draft.cleanup.disabled) ---
  const isOn = (name: string) => !draft!.cleanup.disabled.includes(name);
  const toggleTarget = (name: string) => {
    const dis = new Set(draft!.cleanup.disabled);
    if (dis.has(name)) dis.delete(name);
    else dis.add(name);
    update({ cleanup: { ...draft!.cleanup, disabled: [...dis] } });
  };
  const setGroupAll = (targets: CleanupTarget[], on: boolean) => {
    const dis = new Set(draft!.cleanup.disabled);
    for (const t of targets) {
      if (on) dis.delete(t.name);
      else dis.add(t.name);
    }
    update({ cleanup: { ...draft!.cleanup, disabled: [...dis] } });
  };
  const selectAll = () => update({ cleanup: { ...draft!.cleanup, disabled: [] } });
  const clearAll = () =>
    update({
      cleanup: {
        ...draft!.cleanup,
        disabled: catalog.filter((t) => t.category !== 'custom').map((t) => t.name),
      },
    });

  return (
    <div className="screen">
      <header className="screen-head">
        <h1>
          Regras &amp; Configuração {configQ.revalidating && <RevalidatingBadge />}
        </h1>
        <p>Watch paths, alvos de limpeza e regras de tiering. Salvar grava o config.toml.</p>
        <div className="actions">
          <button onClick={revert} disabled={!dirty}>
            ↺ Reverter
          </button>
          <button className="primary" onClick={save} disabled={!draft}>
            💾 Salvar
          </button>
        </div>
        {saved && <div className="callout">Config salvo.</div>}
        {saveErr && <div className="error">{saveErr}</div>}
      </header>

      {!draft ? (
        configQ.error ? (
          <div className="error">Falha ao carregar config: {configQ.error}</div>
        ) : (
          <RulesSkeleton />
        )
      ) : (
        <>
          <section className="cfg-section">
            <h2>Watch paths</h2>
            <textarea
              className="cfg-textarea"
              value={draft.watch.paths.join('\n')}
              onChange={(e) =>
                update({
                  watch: {
                    ...draft.watch,
                    paths: e.target.value.split('\n').map((s) => s.trim()).filter(Boolean),
                  },
                })
              }
              rows={3}
            />
            <label className="field-label">Ignore globs</label>
            <input
              className="cfg-input"
              value={draft.watch.ignore_globs.join(', ')}
              onChange={(e) =>
                update({
                  watch: {
                    ...draft.watch,
                    ignore_globs: e.target.value.split(',').map((s) => s.trim()).filter(Boolean),
                  },
                })
              }
            />
            <label className="rule-flags" style={{ marginTop: 8 }}>
              <input
                type="checkbox"
                checked={draft.watch.realtime}
                onChange={(e) =>
                  update({ watch: { ...draft.watch, realtime: e.target.checked } })
                }
              />
              Indexação em tempo real (file-watcher)
            </label>
            <p className="muted">
              Mantém o índice sincronizado sem re-scan completo. Aplica-se no próximo início
              do daemon.
            </p>
          </section>

          <section className="cfg-section">
            <div className="cfg-section-head">
              <h2>Alvos de limpeza</h2>
              <div className="actions">
                <button onClick={selectAll}>✓ Selecionar tudo</button>
                <button onClick={clearAll}>✕ Limpar seleção</button>
              </div>
            </div>
            <p className="muted">
              Dependências/regeneráveis descartados ao mover/comprimir. Toggle por alvo e por categoria.
            </p>

            <div className="accordion">
              {groups.map(({ group, targets }) => {
                const onCount = targets.filter((t) => isOn(t.name)).length;
                const allOn = onCount === targets.length;
                return (
                  <div className="accordion-item" key={group}>
                    <div className="accordion-head">
                      <button
                        className="accordion-toggle"
                        onClick={() => setOpenGroups((s) => ({ ...s, [group]: !s[group] }))}
                      >
                        <span className="accordion-caret">{openGroups[group] ? '▾' : '▸'}</span>
                        {group}
                        <span className="muted">
                          {' '}
                          ({onCount}/{targets.length})
                        </span>
                      </button>
                      <label className="accordion-check">
                        <input
                          type="checkbox"
                          checked={allOn}
                          onChange={() => setGroupAll(targets, !allOn)}
                        />
                        Selecionar tudo
                      </label>
                    </div>
                    {openGroups[group] && (
                      <div className="accordion-body">
                        {targets.map((t) => (
                          <label className="target-row" key={t.name}>
                            <input
                              type="checkbox"
                              checked={isOn(t.name)}
                              onChange={() => toggleTarget(t.name)}
                            />
                            <span className="target-info">
                              <span className="target-name">
                                {t.name}
                                <span className="chip cat">{t.category}</span>
                                {!t.regenerable && <span className="chip warn-chip">cuidado</span>}
                              </span>
                              {t.description && <span className="muted target-desc">{t.description}</span>}
                              <span className="target-patterns">
                                {t.patterns.map((p) => (
                                  <span className="chip" key={p}>
                                    {p}
                                  </span>
                                ))}
                              </span>
                            </span>
                          </label>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>

            <details className="custom-targets">
              <summary>Alvos customizados (avançado)</summary>
              <p className="muted">Nomes simples separados por vírgula. Somam-se ao catálogo embutido.</p>
              <input
                className="cfg-input"
                value={draft.cleanup.targets.join(', ')}
                onChange={(e) =>
                  update({
                    cleanup: {
                      ...draft.cleanup,
                      targets: e.target.value.split(',').map((s) => s.trim()).filter(Boolean),
                    },
                  })
                }
              />
            </details>
          </section>

          <section className="cfg-section">
            <h2>Regras de tiering</h2>
            {draft.rules.map((r, idx) => (
              <div className="rule-card" key={idx}>
                <input
                  className="cfg-input rule-name"
                  value={r.name}
                  onChange={(e) => {
                    const rules = [...draft.rules];
                    rules[idx] = { ...r, name: e.target.value };
                    update({ rules });
                  }}
                />
                <div className="rule-grid">
                  <label>
                    glob
                    <input
                      className="cfg-input"
                      value={r.match_glob}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, match_glob: e.target.value };
                        update({ rules });
                      }}
                    />
                  </label>
                  <label>
                    inativo (dias)
                    <input
                      className="cfg-input"
                      type="number"
                      value={r.inactive_days}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, inactive_days: Number(e.target.value) };
                        update({ rules });
                      }}
                    />
                  </label>
                  <label>
                    de tier
                    <select
                      className="cfg-input"
                      value={r.from_tier}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, from_tier: e.target.value as Config['rules'][0]['from_tier'] };
                        update({ rules });
                      }}
                    >
                      <option value="fast">fast</option>
                      <option value="slow">slow</option>
                      <option value="archive">archive</option>
                    </select>
                  </label>
                  <label>
                    para tier
                    <select
                      className="cfg-input"
                      value={r.to_tier}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, to_tier: e.target.value as Config['rules'][0]['to_tier'] };
                        update({ rules });
                      }}
                    >
                      <option value="fast">fast</option>
                      <option value="slow">slow</option>
                      <option value="archive">archive</option>
                    </select>
                  </label>
                </div>
                <div className="rule-flags">
                  <label>
                    <input
                      type="checkbox"
                      checked={r.cleanup_deps}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, cleanup_deps: e.target.checked };
                        update({ rules });
                      }}
                    />
                    limpar deps
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={r.junction}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, junction: e.target.checked };
                        update({ rules });
                      }}
                    />
                    junction
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={r.compress}
                      onChange={(e) => {
                        const rules = [...draft.rules];
                        rules[idx] = { ...r, compress: e.target.checked };
                        update({ rules });
                      }}
                    />
                    comprimir
                  </label>
                </div>
              </div>
            ))}
          </section>
        </>
      )}
    </div>
  );
}
