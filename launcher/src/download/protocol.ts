import type { Text } from '../i18n/error';
import type { VersionManifestEntry } from '../mojang/types';

export type InstallRequest =
  /** full: re-hash every present file (otherwise files of the right size are trusted). */
  | { type: 'install'; entry: VersionManifestEntry; full: boolean }
  | { type: 'cancel' };

export interface InstallProgress {
  totalFiles: number;
  doneFiles: number;
  /** Files that were already present and valid. */
  skippedFiles: number;
  totalBytes: number;
  doneBytes: number;
}

export type InstallEvent =
  /** Messages are keys (see src/i18n): the page shows them in its language. */
  | { type: 'status'; text: Text }
  | ({ type: 'progress' } & InstallProgress)
  | ({ type: 'done'; versionId: string; runtime: string } & InstallProgress)
  | { type: 'error'; text: Text; failures: string[] };
