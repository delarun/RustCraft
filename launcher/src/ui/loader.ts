// The mod loader choice next to the version: Vanilla or Fabric (remembered).
// meta.fabricmc.net is asked for the version's loader builds only once Fabric
// is chosen, so players who never use mods never contact it. The newest
// stable build is used; the last one found per version is remembered, so an
// installed Fabric also starts offline.

import { onLangChange, translate, type Text } from '../i18n';
import { errorText } from '../i18n/error';
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

export class LoaderView {
  private wantsFabric = prefs.get('loader') === 'fabric';
  private game = '';
  private availability: Availability = { kind: 'checking' };
  private lookups = new Map<string, Promise<string | null>>();
  private ui = {
    vanilla: $<HTMLInputElement>('loader-vanilla'),
    fabric: $<HTMLInputElement>('loader-fabric'),
    note: $('loader-note'),
  };

  constructor(
    private onChange: () => void,
    private modCount: () => number,
  ) {
    for (const input of [this.ui.vanilla, this.ui.fabric]) {
      input.addEventListener('change', () => {
        this.wantsFabric = this.ui.fabric.checked;
        prefs.set('loader', this.wantsFabric ? 'fabric' : 'vanilla');
        if (this.availability.kind === 'failed') this.availability = { kind: 'checking' };
        void this.lookUp();
        this.render();
        this.onChange();
      });
    }
    onLangChange(() => this.render());
    this.render();
  }

  /** Whether the player chose Fabric (whether or not it is available). */
  get fabric(): boolean {
    return this.wantsFabric;
  }

  /** The Fabric Loader build to launch with; undefined for vanilla. */
  get fabricLoader(): string | undefined {
    return this.wantsFabric && this.availability.kind === 'available' ? this.availability.loader : undefined;
  }

  set disabled(value: boolean) {
    this.ui.vanilla.disabled = value;
    this.ui.fabric.disabled = value;
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
    this.ui.vanilla.checked = !this.wantsFabric;
    this.ui.fabric.checked = this.wantsFabric;
    const available = this.availability.kind === 'available' ? this.availability : null;
    const problem = this.problem();
    let text: Text | null = null;
    if (this.wantsFabric && available) {
      text = { key: 'loaderReady', args: [available.loader, this.modCount(), available.offline] };
    } else if (problem) {
      text = problem;
    }
    this.ui.note.textContent = text ? translate(text) : '';
    this.ui.note.classList.toggle('error', problem !== null && this.availability.kind !== 'checking');
  }
}
