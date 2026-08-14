// Cache em memória + hook useCached (stale-while-revalidate lite).
//
// Objetivo: "render first". Componentes lêem o cache de forma síncrona no primeiro
// render (useState initializer), então dados já vistos aparecem na hora; só buscamos
// de novo quando o cache está vencido (ttlMs) ou quando uma dep mudou (ex.: refreshKey).
// Sem dependências externas — ~80 linhas, suficiente para o app local.

import { useCallback, useEffect, useRef, useState } from 'react';

interface Entry<T> {
  data: T;
  ts: number;
}

const store = new Map<string, Entry<unknown>>();

/** Lê um valor do cache (ou undefined). */
export function readCache<T>(key: string): T | undefined {
  const e = store.get(key);
  return e ? (e.data as T) : undefined;
}

/** Grava um valor no cache marcando "agora" como freshness. */
export function writeCache<T>(key: string, data: T): void {
  store.set(key, { data, ts: Date.now() });
}

/** Limpa chaves iguais a `key` ou que começam com `prefix + ':'`. Sem args = limpa tudo. */
export function invalidate(keyOrPrefix?: string): void {
  if (keyOrPrefix === undefined) {
    store.clear();
    return;
  }
  for (const k of [...store.keys()]) {
    if (k === keyOrPrefix || k.startsWith(keyOrPrefix + ':')) store.delete(k);
  }
}

export interface UseCachedOpts {
  /** Quanto tempo o cache é considerado fresco (sem refetch). Default 60s. */
  ttlMs?: number;
  /** Mudanças aqui forçam revalidação em background (cache continua visível). */
  deps?: unknown[];
}

export interface UseCachedResult<T> {
  data: T | undefined;
  error: string | undefined;
  /** true só quando não há cache algum (mostrar skeleton). */
  loading: boolean;
  /** true enquanto revalida em background (cache já está sendo exibido). */
  revalidating: boolean;
  /** Força uma revalidação agora. */
  refresh: () => void;
  /** Atualiza cache + estado (updates otimistas pós-ação). */
  setData: (next: T | ((prev: T | undefined) => T)) => void;
}

/**
 * Hook de fetch com cache SWR.
 *
 * - Render-first: o estado inicial vem do cache; se existir, a tela pinta imediatamente.
 * - `fetcher` é chamado de forma síncrona quando cache está ausente/vencido.
 * - Mudança em `opts.deps` revalida em background (mantém o cache visível).
 *
 * Nota: `fetcher` é intencionalmente fora do array de deps do effect (identidade
 * instável em callers inline); use `key`/`deps` para controlar revalidação.
 */
export function useCached<T>(
  key: string,
  fetcher: () => Promise<T>,
  opts: UseCachedOpts = {},
): UseCachedResult<T> {
  const { ttlMs = 60_000, deps = [] } = opts;

  const [data, setDataState] = useState<T | undefined>(() => readCache<T>(key));
  const [error, setError] = useState<string | undefined>();
  const [revalidating, setRevalidating] = useState(false);
  const [tick, setTick] = useState(0);
  // Primeira execução deste effect para a key atual respeita TTL; as demais forçam.
  const firstRun = useRef(true);
  // Quando a key muda (ex.: navegação de pasta), resemeia do cache ainda no render
  // (padrão "adjust state during render" do React) — evita 1 frame com dados da key anterior.
  const trackedKey = useRef(key);
  if (trackedKey.current !== key) {
    trackedKey.current = key;
    setDataState(readCache<T>(key));
    setError(undefined);
    firstRun.current = true;
  }

  useEffect(() => {
    let cancelled = false;

    // Sempre sincroniza o estado com o cache (garante render-first na remontagem).
    const cached = readCache<T>(key);
    if (cached !== undefined) setDataState(cached);

    const fresh = cached !== undefined && Date.now() - (store.get(key)?.ts ?? 0) <= ttlMs;
    const force = !firstRun.current; // runs subsequentes (deps/tick mudaram)
    firstRun.current = false;

    if (fresh && !force) return;

    setRevalidating(true);
    fetcher()
      .then((d) => {
        if (cancelled) return;
        writeCache(key, d);
        setDataState(d);
        setError(undefined);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setRevalidating(false);
      });

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, ttlMs, tick, ...deps]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);

  const setData = useCallback(
    (next: T | ((prev: T | undefined) => T)) => {
      setDataState((prev) => {
        const value = typeof next === 'function' ? (next as (p: T | undefined) => T)(prev) : next;
        writeCache(key, value);
        return value;
      });
    },
    [key],
  );

  return {
    data,
    error,
    loading: data === undefined && error === undefined,
    revalidating,
    refresh,
    setData,
  };
}
