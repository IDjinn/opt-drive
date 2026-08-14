// Cliente da API do daemon (REST + WebSocket).
import type { CleanupTarget, Config, DirEntry, Drive, IndexStatus, Plan, RunReport } from './types';

declare global {
  interface Window {
    optDrive?: { baseUrl: string };
  }
}

/** Base URL do daemon, injetada pelo processo main via preload.
 *  Override de dev: `?api=<url>` na query string ou `localStorage.optDrive.apiBase`
 *  (permite testar o renderer fora do Electron). */
export function apiBase(): string {
  const q = new URLSearchParams(window.location.search).get('api');
  return (
    window.optDrive?.baseUrl?.replace(/\/$/, '') ??
    q?.replace(/\/$/, '') ??
    localStorage.getItem('optDrive.apiBase')?.replace(/\/$/, '') ??
    ''
  );
}

function wsBase(): string {
  return (apiBase() || '').replace(/^http/, 'ws');
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${apiBase()}${path}`, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`${res.status} ${res.statusText} — ${text}`);
  }
  return res.json() as Promise<T>;
}

export const api = {
  drives: () => req<Drive[]>('/api/drives'),
  config: () => req<Config>('/api/config'),
  saveConfig: (cfg: Config) =>
    req<{ saved: boolean }>('/api/config', { method: 'PUT', body: JSON.stringify(cfg) }),
  indexStatus: () => req<IndexStatus>('/api/index/status'),
  indexRun: () =>
    req<{ started: boolean; canceled: boolean }>('/api/index/run', { method: 'POST' }),
  indexCancel: () =>
    req<{ canceled: boolean }>('/api/index/cancel', { method: 'POST' }),
  browse: (path: string) => req<DirEntry[]>('/api/browse?path=' + encodeURIComponent(path)),
  cleanupCatalog: () => req<CleanupTarget[]>('/api/cleanup/catalog'),
  tierPreview: () => req<Plan>('/api/tier/preview'),
  tierApply: () =>
    req<{ report: RunReport; journal: string }>('/api/tier/apply', { method: 'POST' }),
};

/** Abre o WebSocket de eventos e chama `onEvent` para cada mensagem. */
export function openEvents(onEvent: (e: unknown) => void): () => void {
  const url = `${wsBase()}/api/events`;
  if (!url.startsWith('ws')) {
    // sem base URL — nada a fazer
    return () => {};
  }
  let ws: WebSocket | null = null;
  let closed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  const connect = () => {
    ws = new WebSocket(url);
    ws.onmessage = (ev) => {
      try {
        onEvent(JSON.parse(ev.data));
      } catch {
        /* ignora payload inválido */
      }
    };
    ws.onclose = () => {
      if (!closed) reconnectTimer = setTimeout(connect, 2000);
    };
    ws.onerror = () => ws?.close();
  };
  connect();

  return () => {
    closed = true;
    if (reconnectTimer) clearTimeout(reconnectTimer);
    ws?.close();
  };
}

/** Formata bytes em string legível. */
export function fmtBytes(b: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const TB = GB * 1024;
  if (b >= TB) return `${(b / TB).toFixed(2)} TB`;
  if (b >= GB) return `${(b / GB).toFixed(2)} GB`;
  if (b >= MB) return `${(b / MB).toFixed(2)} MB`;
  if (b >= KB) return `${(b / KB).toFixed(1)} KB`;
  return `${b} B`;
}
