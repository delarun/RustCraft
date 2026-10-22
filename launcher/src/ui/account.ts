// The account card: Microsoft sign-in by device code, the license check, the
// signed-in profile, sign-out and the Azure client ID setting. Messages are
// kept as keys, so a language switch re-renders them.

import {
  clearAccount,
  freshAccount,
  licenseProblem,
  loadAccount,
  saveAccount,
  signInWithMsa,
  type Account,
  type Step,
} from '../auth/account';
import { buildClientId, clientId, configuredClientId, isClientId, setConfiguredClientId } from '../auth/config';
import { dashedUuid } from '../auth/minecraft';
import { pollDeviceCode, startDeviceCode } from '../auth/msa';
import { onLangChange, translate, type MessageKey, type Text } from '../i18n';
import { errorText } from '../i18n/error';
import { $ } from './dom';

const STEPS: Record<Step, MessageKey> = {
  msa: 'stepMsa',
  xbox: 'stepXbox',
  xsts: 'stepXsts',
  minecraft: 'stepMinecraft',
  license: 'stepLicense',
  profile: 'stepProfile',
};

export class AccountView {
  private account: Account | null = loadAccount();
  private login: AbortController | null = null;
  private countdown = 0;
  private expiresAt = 0;
  private status: { text: Text; error: boolean } | null = null;
  private clientIdError = false;
  private ui = {
    signedOut: $('account-signed-out'),
    device: $('account-device'),
    deviceLink: $<HTMLAnchorElement>('device-link'),
    deviceCode: $('device-code'),
    deviceCopy: $<HTMLButtonElement>('device-copy'),
    deviceCancel: $<HTMLButtonElement>('device-cancel'),
    deviceExpiry: $('device-expiry'),
    signedIn: $('account-signed-in'),
    avatar: $('profile-avatar'),
    name: $('profile-name'),
    uuid: $('profile-uuid'),
    license: $('license'),
    loginButton: $<HTMLButtonElement>('login'),
    logout: $<HTMLButtonElement>('logout'),
    status: $('account-status'),
    clientId: $<HTMLInputElement>('client-id'),
    clientIdSave: $<HTMLButtonElement>('client-id-save'),
    clientIdNote: $('client-id-note'),
    noClientId: $('no-client-id'),
  };

  constructor(private onChange: () => void) {
    this.ui.loginButton.addEventListener('click', () => void this.signIn());
    this.ui.deviceCancel.addEventListener('click', () => this.login?.abort());
    this.ui.deviceCopy.addEventListener('click', () => void navigator.clipboard?.writeText(this.ui.deviceCode.textContent ?? ''));
    this.ui.logout.addEventListener('click', () => this.signOut());
    this.ui.clientIdSave.addEventListener('click', () => this.saveClientId());
    this.ui.clientId.value = configuredClientId() ?? '';
    this.ui.clientId.placeholder = buildClientId() ?? clientId() ?? '00000000-0000-0000-0000-000000000000';
    onLangChange(() => this.render());
    this.render();
    // Renew a stale session in the background, so Play starts at once.
    if (this.account) void this.ensureFresh().catch(() => {});
  }

  get current(): Account | null {
    return this.account;
  }

  /** Why the account cannot play (a message key), or null. */
  problem(): MessageKey | null {
    if (this.login) return 'finishSignIn';
    if (!this.account) return clientId() ? 'signInToPlay' : 'signInUnavailable';
    return licenseProblem(this.account);
  }

  /** The account with a valid Minecraft token (renewed if needed). */
  async ensureFresh(): Promise<Account> {
    if (!this.account) throw new Error('not signed in');
    try {
      const renewed = await freshAccount(this.account, (step) => this.setStatus({ key: STEPS[step] }));
      if (renewed !== this.account) this.store(renewed);
      this.setStatus(null);
      return renewed;
    } catch (error) {
      this.setStatus(errorText(error), true);
      if ((error as { code?: string }).code === 'relogin') this.signOut(false);
      throw error;
    }
  }

  private setStatus(text: Text | null, error = false) {
    this.status = text && { text, error };
    this.render();
  }

  private async signIn() {
    if (this.login) return;
    const id = clientId();
    if (!id) {
      this.setStatus({ key: 'noClientId' }, true);
      return;
    }
    const abort = (this.login = new AbortController());
    this.setStatus({ key: 'requestingCode' });
    this.onChange();
    try {
      const code = await startDeviceCode(id, abort.signal);
      this.ui.deviceCode.textContent = code.userCode;
      this.ui.deviceLink.href = code.verificationUri;
      this.ui.deviceLink.textContent = code.verificationUri.replace(/^https:\/\/(www\.)?/, '');
      this.expiresAt = code.expiresAt;
      this.ui.device.hidden = false;
      this.setStatus(null);
      this.tick();
      this.countdown = window.setInterval(() => this.tick(), 1000);
      const msa = await pollDeviceCode(id, code, abort.signal);
      this.stopDevice();
      const account = await signInWithMsa(id, msa, (step) => this.setStatus({ key: STEPS[step] }));
      this.store(account);
      const problem = licenseProblem(account);
      this.setStatus(problem && { key: problem }, problem !== null);
    } catch (error) {
      this.stopDevice();
      this.setStatus(abort.signal.aborted ? { key: 'signInCancelled' } : errorText(error), !abort.signal.aborted);
    } finally {
      this.login = null;
      this.render();
      this.onChange();
    }
  }

  private tick() {
    const left = Math.max(0, Math.round((this.expiresAt - Date.now()) / 1000));
    this.ui.deviceExpiry.textContent = translate({ key: 'codeExpiresIn', args: [Math.floor(left / 60), String(left % 60).padStart(2, '0')] });
  }

  private stopDevice() {
    clearInterval(this.countdown);
    this.ui.device.hidden = true;
  }

  private signOut(announce = true) {
    this.account = null;
    clearAccount();
    this.setStatus(announce ? { key: 'signedOut' } : this.status?.text ?? null, !announce && (this.status?.error ?? false));
    this.onChange();
  }

  private store(account: Account) {
    this.account = account;
    saveAccount(account);
    this.render();
    this.onChange();
  }

  private saveClientId() {
    const value = this.ui.clientId.value.trim();
    this.clientIdError = value !== '' && !isClientId(value);
    if (!this.clientIdError) {
      setConfiguredClientId(value || null);
      if (this.account && this.account.clientId !== clientId()) this.signOut(false);
    }
    this.render();
    this.onChange();
  }

  private render() {
    const account = this.account;
    const signingIn = this.login !== null;
    const deviceShown = !this.ui.device.hidden;
    this.ui.signedOut.hidden = deviceShown || account !== null;
    this.ui.loginButton.disabled = signingIn || !clientId();
    this.ui.signedIn.hidden = account === null || deviceShown;
    if (account) {
      const name = account.profile?.name;
      this.ui.name.textContent = name ?? translate({ key: 'noJavaProfile' });
      this.ui.avatar.textContent = (name ?? '?').slice(0, 1).toUpperCase();
      this.ui.uuid.textContent = account.profile ? dashedUuid(account.profile.id) : '';
      const problem = licenseProblem(account);
      this.ui.license.textContent = translate({ key: problem ?? 'licenseOk' });
      this.ui.license.classList.toggle('error', problem !== null);
    }
    if (deviceShown) this.tick();
    this.ui.status.textContent = this.status ? translate(this.status.text) : '';
    this.ui.status.classList.toggle('error', this.status?.error ?? false);

    const id = clientId();
    const note: Text = this.clientIdError
      ? { key: 'clientIdInvalid' }
      : !id
        ? { key: 'noClientId' }
        : { key: configuredClientId() ? 'clientIdFromSettings' : buildClientId() ? 'clientIdFromBuild' : 'clientIdDefault', args: [id] };
    this.ui.clientIdNote.textContent = translate(note);
    this.ui.clientIdNote.classList.toggle('error', this.clientIdError || !id);
    this.ui.noClientId.hidden = id !== null || account !== null;
  }
}
