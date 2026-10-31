// The mod loader choice next to the version: Vanilla, Fabric or Horizon (remembered).
// Horizon is not a loader but a game of its own (src/horizon.ts) that plays one version.
// meta.fabricmc.net is asked for the version's loader builds only once Fabric
// is chosen, so players who never use mods never contact it. The newest
// stable build is used; the last one found per version is remembered, so an
// installed Fabric also starts offline.

import { onLangChange, translate, type Text } from '../i18n';
import { errorText } from '../i18n/error';
import { horizonInfo, type HorizonInfo } from '../horizon';
import { fabricLoadersUrl, pickLoader, type FabricLoaderEntry } from '../mojang/fabric';
import { $, prefs } from './dom';

type Availability =
  | { kind: 'checking' }
  | { kind: 'available'; loader: string; offline: boolean }
  | { kind: 'unavailable' }
  | { kind: 'failed'; error: Text };

/** The newest stable Fabric Loader for a game version; null if Fabric has none. */
async function fetchLoader(game: string): Promise<string | null> {
  const response = await fetch(fabricLoadersUrl(game));
  // Versions Fabric does not know: 400 with an empty list.
  if (response.status === 400) return null;
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return pickLoader((await response.json()) as FabricLoaderEntry[]);
}

type Mode = 'vanilla' | 'fabric' | 'horizon';

export class LoaderView {
  private mode: Mode = (['fabric', 'horizon'] as const).find((m) => m === prefs.get('loader')) ?? 'vanilla';
  /** What the site serves of Horizon: undefined until asked, null when nothing. */
  private horizonServed: HorizonInfo | null | undefined;
  private horizonError: Text | null = null;
  private game = '';
  private availability: Availability = { kind: 'checking' };
  private lookups = new Map<string, Promise<string | null>>();
  private ui = {
    vanilla: $<HTMLInputElement>('loader-vanilla'),
    fabric: $<HTMLInputElement>('loader-fabric'),
    horizon: $<HTMLInputElement>('loader-horizon'),
    note: $('loader-note'),
  };

  constructor(
    private onChange: () => void,
    private modCount: () => number,
  ) {
    for (const input of [this.ui.vanilla, this.ui.fabric, this.ui.horizon]) {
      input.addEventListener('change', () => {
        this.mode = this.ui.fabric.checked ? 'fabric' : this.ui.horizon.checked ? 'horizon' : 'vanilla';
        prefs.set('loader', this.mode);
        void this.lookUpHorizon();
        if (this.availability.kind === 'failed') this.availability = { kind: 'checking' };
        void this.lookUp();
        this.render();
        this.onChange();
      });
    }
    onLangChange(() => this.render());
    void this.lookUpHorizon();
    this.render();
  }

  private get wantsFabric(): boolean {
    return this.mode === 'fabric';
  }

  /** Whether the player chose Fabric (whether or not it is available). */
  get fabric(): boolean {
    return this.wantsFabric;
  }

  /** Horizon to play, when the player chose it and the site has it. */
  get horizon(): HorizonInfo | null {
    return this.mode === 'horizon' ? (this.horizonServed ?? null) : null;
  }

  /** Whether the player chose Horizon (whether or not it is available). */
  get wantsHorizon(): boolean {
    return this.mode === 'horizon';
  }

  private async lookUpHorizon() {
    if (this.mode !== 'horizon' || this.horizonServed !== undefined) return;
    try {
      this.horizonServed = await horizonInfo();
      this.horizonError = null;
    } catch (error) {
      this.horizonError = errorText(error);
    }
    this.render();
    this.onChange();
  }

  /** The Fabric Loader build to launch with; undefined for vanilla. */
  get fabricLoader(): string | undefined {
    return this.wantsFabric && this.availability.kind === 'available' ? this.availability.loader : undefined;
  }

  set disabled(value: boolean) {
    this.ui.vanilla.disabled = value;
    this.ui.fabric.disabled = value;
    this.ui.horizon.disabled = value;
  }

  setGameVersion(game: string) {
    if (game === this.game) return;
    this.game = game;
    this.availability = { kind: 'checking' };
    void this.lookUp();
    this.render();
  }

  /** Why the chosen loader cannot start, or null. */
  problem(): Text | null {
    if (this.mode === 'horizon') {
      if (this.horizonError) return { key: 'horizonFailed', args: [translate(this.horizonError)] };
      if (this.horizonServed === undefined) return { key: 'horizonChecking' };
      if (this.horizonServed === null) return { key: 'horizonUnavailable' };
      if (this.game && this.game !== this.horizonServed.version) return { key: 'horizonVersion', args: [this.horizonServed.version] };
      return null;
    }
    if (!this.wantsFabric || !this.game) return null;
    switch (this.availability.kind) {
      case 'checking':
        return { key: 'loaderChecking', args: [this.game] };
      case 'unavailable':
        return { key: 'loaderUnavailable', args: [this.game] };
      case 'failed':
        return { key: 'loaderFailed', args: [translate(this.availability.error)] };
      case 'available':
        return null;
    }
  }

  private async lookUp() {
    const game = this.game;
    if (!this.wantsFabric || !game || this.availability.kind !== 'checking') return;
    let lookup = this.lookups.get(game);
    if (!lookup) {
      lookup = fetchLoader(game);
      this.lookups.set(game, lookup);
      lookup.catch(() => this.lookups.delete(game));
    }
    let availability: Availability;
    try {
      const loader = await lookup;
      if (loader) prefs.set(`fabricLoader.${game}`, loader);
      availability = loader ? { kind: 'available', loader, offline: false } : { kind: 'unavailable' };
    } catch (error) {
      const known = prefs.get(`fabricLoader.${game}`);
      availability = known ? { kind: 'available', loader: known, offline: true } : { kind: 'failed', error: errorText(error) };
    }
    if (game !== this.game) return;
    this.availability = availability;
    this.render();
    this.onChange();
  }

  render() {
    this.ui.vanilla.checked = this.mode === 'vanilla';
    this.ui.fabric.checked = this.mode === 'fabric';
    this.ui.horizon.checked = this.mode === 'horizon';
    const available = this.availability.kind === 'available' ? this.availability : null;
    const problem = this.problem();
    let text: Text | null = null;
    if (this.mode === 'horizon' && !problem) {
      text = { key: 'horizonReady', args: [this.horizonServed!.version] };
    } else if (this.wantsFabric && available) {
      text = { key: 'loaderReady', args: [available.loader, this.modCount(), available.offline] };
    } else if (problem) {
      text = problem;
    }
    this.ui.note.textContent = text ? translate(text) : '';
    const checking = this.mode === 'horizon' ? this.horizonServed === undefined && !this.horizonError : this.availability.kind === 'checking';
    this.ui.note.classList.toggle('error', problem !== null && !checking);
  }
}
