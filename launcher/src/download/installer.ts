import type { VersionManifestEntry } from '../mojang/types';
import type { InstallEvent, InstallRequest } from './protocol';

export type InstallResult = Extract<InstallEvent, { type: 'done' | 'error' }>;

/** Main-thread handle to the download worker. */
export class Installer {
  private worker = new Worker(new URL('./download.worker.ts', import.meta.url), { type: 'module', name: 'installer' });
  private listener: ((event: InstallEvent) => void) | null = null;

  constructor() {
    this.worker.onmessage = (event: MessageEvent<InstallEvent>) => this.listener?.(event.data);
    this.worker.onerror = (event) => this.listener?.({ type: 'error', text: { key: 'text', args: [event.message] }, failures: [] });
  }

  /** Installs (or checks) a version; resolves with the final event. */
  install(entry: VersionManifestEntry, full: boolean, onEvent: (event: InstallEvent) => void): Promise<InstallResult> {
    return new Promise((resolve) => {
      this.listener = (event) => {
        onEvent(event);
        if (event.type === 'done' || event.type === 'error') {
          this.listener = null;
          resolve(event);
        }
      };
      this.send({ type: 'install', entry, full });
    });
  }

  cancel() {
    this.send({ type: 'cancel' });
  }

  private send(request: InstallRequest) {
    this.worker.postMessage(request);
  }
}
