// The Multiplayer settings: whether rustcraft-bridge is running on this
// computer, where to get it, and its address. The bridge is probed only when
// the section opens or on "Check again", never on page load: the first request
// to 127.0.0.1 may make Chrome ask for local network access.

import { onLangChange, translate, type Text } from '../i18n';
import { bridgeStatus } from '../jvm/net';
import { BRIDGE_DOWNLOAD_URL, bridgeAddress, DEFAULT_BRIDGE, parseBridgeAddress } from '../net/bridge';
import { $, prefs } from './dom';

type State = { kind: 'idle' } | { kind: 'checking' } | { kind: 'running'; version: string } | { kind: 'missing' };

export class MultiplayerView {
  private state: State = { kind: 'idle' };
  private invalid = false;
  private probe = 0;
  private ui = {
    section: $<HTMLDetailsElement>('multiplayer'),
    status: $('bridge-status'),
    row: $('bridge-status').parentElement!,
    check: $<HTMLButtonElement>('bridge-check'),
    download: $<HTMLAnchorElement>('bridge-download'),
    address: $<HTMLInputElement>('bridge-address'),
    save: $<HTMLButtonElement>('bridge-save'),
    note: $('bridge-note'),
  };

  constructor() {
    this.ui.download.href = BRIDGE_DOWNLOAD_URL;
    this.ui.address.placeholder = DEFAULT_BRIDGE;
    this.ui.address.value = prefs.get('netBridge') ?? '';
    this.ui.section.addEventListener('toggle', () => {
      if (this.ui.section.open) void this.check();
    });
    this.ui.check.addEventListener('click', () => void this.check());
    this.ui.save.addEventListener('click', () => this.saveAddress());
    this.ui.address.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') this.saveAddress();
    });
    onLangChange(() => this.render());
    this.render();
  }

  /** The bridge address the game should use. */
  get address(): string {
    return bridgeAddress(prefs.get('netBridge'));
  }

  async check() {
    const probe = ++this.probe;
    this.state = { kind: 'checking' };
    this.render();
    const status = await bridgeStatus(this.address);
    if (probe !== this.probe) return;
    this.state = status ? { kind: 'running', version: status.version } : { kind: 'missing' };
    this.render();
  }

  private saveAddress() {
    const value = this.ui.address.value.trim();
    const parsed = value ? parseBridgeAddress(value) : null;
    this.invalid = value !== '' && parsed === null;
    if (!this.invalid) {
      prefs.set('netBridge', parsed ?? '');
      this.ui.address.value = parsed ?? '';
      void this.check();
    }
    this.render();
  }

  private render() {
    const state = this.state;
    const text: Text | null =
      state.kind === 'checking'
        ? { key: 'bridgeChecking' }
        : state.kind === 'running'
          ? { key: 'bridgeRunning', args: [state.version] }
          : state.kind === 'missing'
            ? { key: 'bridgeNotFound' }
            : null;
    this.ui.status.textContent = text ? translate(text) : '';
    this.ui.row.className = `bridge-status ${state.kind}`;
    this.ui.check.disabled = state.kind === 'checking';
    this.ui.note.textContent = translate(this.invalid ? { key: 'bridgeAddressInvalid' } : { key: 'bridgeAddressInUse', args: [this.address] });
    this.ui.note.classList.toggle('error', this.invalid);
  }
}
