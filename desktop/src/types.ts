// Tipos espelhando os DTOs do daemon (opt-drive-core).

export type Tier = 'fast' | 'slow' | 'archive';
export type DriveKind = 'nvme' | 'ssd' | 'hdd' | 'removable' | 'unknown';

export interface Drive {
  mount: string;
  label: string;
  fs_type: string;
  model: string;
  brand: string;
  total_bytes: number;
  free_bytes: number;
  kind: DriveKind;
  bus: string;
  rotation_rate: number | null;
  tier: Tier;
  read_mbps: number | null;
  write_mbps: number | null;
  speed_frac: number;
}

export interface Rule {
  name: string;
  match_glob: string;
  inactive_days: number;
  min_size: number | null;
  from_tier: Tier;
  to_tier: Tier;
  cleanup_deps: boolean;
  junction: boolean;
  compress: boolean;
}

export interface CleanupConfig {
  targets: string[];
  disabled: string[];
  cleanup_on_compress: boolean;
}

export interface CleanupTarget {
  name: string;
  patterns: string[];
  category: string;
  description: string;
  match_dirs: boolean;
  match_files: boolean;
  regenerable: boolean;
}

export interface DirEntry {
  name: string;
  is_dir: boolean;
  size_bytes: number;
  mtime: number;
  indexed: boolean;
}

export interface WatchConfig {
  paths: string[];
  ignore_globs: string[];
  realtime: boolean;
  debounce_ms: number;
}

export interface DriveConfig {
  path: string;
  tier: Tier;
}

export interface Config {
  drives: DriveConfig[];
  watch: WatchConfig;
  cleanup: CleanupConfig;
  rules: Rule[];
  /** Opcional: daemons antigos não têm esta seção no /api/config. */
  indexer?: IndexerConfig;
}

/// Ajustes do motor de indexação (paralelismo / uso de CPU).
export interface IndexerConfig {
  /** Nº de threads de varredura (0 = automático = nº de CPUs). */
  threads: number;
}

/** Progresso parcial de uma varredura (evento `scan_progress`). */
export interface ScanProgress {
  indexed: number;
  current_dir: string | null;
  total_estimate: number | null;
  elapsed_ms: number;
  bytes: number;
  errors: number;
}

export type Action =
  | {
      kind: 'relocate';
      rule: string;
      src: string;
      dst: string;
      from_drive: string;
      to_drive: string;
      junction: boolean;
      cleanup_deps: boolean;
      compress: boolean;
      size_bytes: number;
      days_inactive: number;
    }
  | {
      kind: 'compress';
      rule: string;
      src: string;
      dst: string;
      cleanup_deps: boolean;
      size_bytes: number;
    };

export interface Plan {
  actions: Action[];
  total_bytes_moved: number;
  total_bytes_cleaned_estimate: number;
}

export interface RunReport {
  dry_run: boolean;
  relocated: number;
  compressed: number;
  bytes_cleaned: number;
  bytes_moved: number;
  errors: number;
  journal_id: string;
}

export interface IndexStatus {
  entries: number;
  last_indexed: number | null;
}

export type DaemonEvent =
  | { type: 'hello'; version: string }
  | { type: 'scan_started'; total_estimate: number | null }
  | { type: 'scan_progress'; indexed: number; current_dir: string | null; total_estimate: number | null; elapsed_ms: number; bytes: number; errors: number }
  | { type: 'scan_done'; stats: { roots_scanned: number; entries_indexed: number; dirs: number; files: number; total_bytes: number } }
  | { type: 'scan_canceled' }
  | { type: 'index_updated'; stats: { upserted: number; removed: number; skipped: number } }
  | { type: 'tier_preview'; plan: Plan }
  | { type: 'tier_started' }
  | { type: 'tier_progress'; desc: string; frac: number }
  | { type: 'tier_done'; report: RunReport };
