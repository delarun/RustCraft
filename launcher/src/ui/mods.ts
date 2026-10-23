// The Mods settings: the jars in mods/ (with what Modrinth told us about
// them), removing them, adding .jar files from this computer (file picker or
// drag and drop), and searching Modrinth for Fabric mods of the selected game
// version, installed with their required dependencies. Modrinth is contacted
// only once the section is opened.

import { lang, onLangChange, t, tr, translate, type Text } from '../i18n';
import { errorText } from '../i18n/error';
import {
  downloadFile,
  modrinthApi,
  projectPageUrl,
  resolveInstall,
  searchMods,
  type ModrinthHit,
  type PlannedMod,
} from '../mods/modrinth';
import { addMod, isModFileName, listMods, removeMod, type InstalledMod } from '../mods/store';
import { $, formatBytes } from './dom';

const SEARCH_DEBOUNCE_MS = 400;

type Search = { kind: 'idle' } | { kind: 'searching' } | { kind: 'done'; hits: ModrinthHit[] } | { kind: 'failed'; error: Text };

export class ModsView {
  private mods: InstalledMod[] = [];
  private game = '';
  private fabric = false;
  private search: Search = { kind: 'idle' };
  private searchFor = '';
  private searchTimer = 0;
  private searchAbort: AbortController | null = null;
  /** The project being installed. */
  private installing: string | null = null;
  private status: { text: Text; error: boolean } | null = null;
  private api = modrinthApi();
  private ui = {
    section: $<HTMLDetailsElement>('mods'),
    vanilla: $('mods-vanilla'),
    list: $('mods-list'),
    empty: $('mods-empty'),
    drop: $('mods-drop'),
    file: $<HTMLInputElement>('mods-file'),
    query: $<HTMLInputElement>('mods-search'),
    status: $('mods-status'),
    results: $('mods-results'),
  };

  constructor(private onChange: () => void) {
    this.ui.section.addEventListener('toggle', () => {
      if (this.ui.section.open && this.search.kind === 'idle') this.runSearch();
    });
    this.ui.query.addEventListener('input', () => {
      clearTimeout(this.searchTimer);
      this.searchTimer = window.setTimeout(() => this.runSearch(), SEARCH_DEBOUNCE_MS);
    });
    this.ui.file.addEventListener('change', () => {
      void this.upload([...(this.ui.file.files ?? [])]);
      this.ui.file.value = '';
    });
    this.ui.drop.addEventListener('dragover', (event) => {
      event.preventDefault();
      this.ui.drop.classList.add('dragging');
    });
    this.ui.drop.addEventListener('dragleave', () => this.ui.drop.classList.remove('dragging'));
    this.ui.drop.addEventListener('drop', (event) => {
      event.preventDefault();
      this.ui.drop.classList.remove('dragging');
      void this.upload([...(event.dataTransfer?.files ?? [])]);
    });
    onLangChange(() => this.render());
    void this.refresh();
  }

  get count(): number {
    return this.mods.length;
  }

  /** The selected game version and whether Fabric (and so the mods) will be loaded. */
  setContext(game: string, fabric: boolean) {
    const searchAgain = game !== this.game && this.search.kind !== 'idle';
    this.game = game;
    this.fabric = fabric;
    if (searchAgain) this.runSearch();
    this.render();
  }

  /** Re-reads mods/ (after "Delete everything", for one). */
  async refresh() {
    try {
      this.mods = await listMods();
    } catch (error) {
      this.mods = [];
      this.setStatus(errorText(error), true);
    }
    this.render();
    this.onChange();
  }

  // --- search ------------------------------------------------------------------------

  private runSearch() {
    if (!this.game) return;
    const text = this.ui.query.value.trim();
    this.searchAbort?.abort();
    const abort = (this.searchAbort = new AbortController());
    this.search = { kind: 'searching' };
    this.searchFor = this.game;
    this.render();
    searchMods(text, this.game, abort.signal).then(
      (result) => {
        if (abort.signal.aborted) return;
        this.search = { kind: 'done', hits: result.hits };
        this.render();
      },
      (error: unknown) => {
        if (abort.signal.aborted) return;
        this.search = { kind: 'failed', error: errorText(error) };
        this.render();
      },
    );
  }

  // --- changes -----------------------------------------------------------------------

  private async install(hit: ModrinthHit) {
    if (this.installing) return;
    this.installing = hit.project_id;
    const game = this.game;
    this.status = { text: { key: 'modsResolving', args: [hit.title] }, error: false };
    this.render();
    try {
      const installed = new Map(this.mods.flatMap((mod) => (mod.modrinth ? [[mod.modrinth.projectId, mod.modrinth.title] as const] : [])));
      const plan = await resolveInstall(hit.project_id, game, installed, this.api);
      for (const [i, mod] of plan.entries()) {
        this.setStatus({ key: 'modsDownloading', args: [mod.project.title, mod.version.version_number, i + 1, plan.length] });
        await this.save(mod);
      }
      await this.refresh();
      this.setStatus({ key: 'modsInstalledDone', args: [hit.title, plan.length - 1, this.fabric] });
    } catch (error) {
      await this.refresh();
      this.setStatus(errorText(error), true);
    } finally {
      this.installing = null;
      this.render();
    }
  }

  private async save({ project, version, file }: PlannedMod) {
    if (!isModFileName(file.filename)) throw new Error(`Not a mod file name: ${file.filename}`);
    const data = await downloadFile(file);
    await addMod(file.filename, data, {
      projectId: project.id,
      versionId: version.id,
      slug: project.slug,
      title: project.title,
      versionNumber: version.version_number,
      gameVersions: version.game_versions,
      iconUrl: project.icon_url,
    });
  }

  private async upload(files: File[]) {
    if (files.length === 0) return;
    const bad = files.find((file) => !isModFileName(file.name));
    if (bad) {
      this.setStatus({ key: 'modsNotJar', args: [bad.name] }, true);
      return;
    }
    try {
      for (const file of files) await addMod(file.name, file);
      this.setStatus({ key: 'modsAdded', args: [files.length, this.fabric] });
    } catch (error) {
      this.setStatus(errorText(error), true);
    }
    await this.refresh();
  }

  private async remove(mod: InstalledMod) {
    try {
      await removeMod(mod.file);
      this.setStatus({ key: 'modsRemoved', args: [mod.modrinth?.title ?? mod.file] });
    } catch (error) {
      this.setStatus(errorText(error), true);
    }
    await this.refresh();
  }

  private setStatus(text: Text | null, error = false) {
    this.status = text && { text, error };
    this.renderStatus();
  }

  // --- rendering ---------------------------------------------------------------------

  private render() {
    this.ui.vanilla.hidden = this.fabric;
    this.ui.query.placeholder = tr('modsSearchPlaceholder', this.game);
    this.ui.empty.hidden = this.mods.length > 0;
    this.ui.list.replaceChildren(...this.mods.map((mod) => this.installedItem(mod)));
    this.renderResults();
    this.renderStatus();
  }

  private renderStatus() {
    this.ui.status.textContent = this.status ? translate(this.status.text) : '';
    this.ui.status.classList.toggle('error', this.status?.error ?? false);
  }

  private installedItem(mod: InstalledMod): HTMLElement {
    const source = mod.modrinth;
    const details = [source?.versionNumber, formatBytes(mod.size), source ? mod.file : null].filter(Boolean).join(' · ');
    const other = source && this.game && !source.gameVersions.includes(this.game) ? tr('modForVersion', source.gameVersions.join(', ')) : null;
    const remove = button(t.modRemove, 'button ghost small danger', () => void this.remove(mod));
    remove.disabled = this.installing !== null;
    return item(source?.iconUrl ?? null, [
      Object.assign(document.createElement('strong'), { textContent: source?.title ?? mod.file }),
      Object.assign(document.createElement('span'), { className: 'muted small', textContent: details }),
      ...(other ? [Object.assign(document.createElement('span'), { className: 'notice small', textContent: other })] : []),
    ], remove);
  }

  private renderResults() {
    const search = this.search;
    const message: Text | null =
      search.kind === 'searching'
        ? { key: 'modsSearching' }
        : search.kind === 'failed'
          ? search.error
          : search.kind === 'done' && search.hits.length === 0
            ? { key: 'modsNoResults', args: [this.searchFor] }
            : null;
    if (message) {
      const p = Object.assign(document.createElement('li'), { className: `status${search.kind === 'failed' ? ' error' : ''}`, textContent: translate(message) });
      this.ui.results.replaceChildren(p);
      return;
    }
    const hits = search.kind === 'done' ? search.hits : [];
    const installed = new Set(this.mods.flatMap((mod) => (mod.modrinth ? [mod.modrinth.projectId] : [])));
    const downloads = new Intl.NumberFormat(lang, { notation: 'compact', maximumFractionDigits: 1 });
    this.ui.results.replaceChildren(
      ...hits.map((hit) => {
        const isInstalled = installed.has(hit.project_id);
        const label = this.installing === hit.project_id ? t.modInstalling : isInstalled ? t.modInstalledMark : t.modInstall;
        const action = button(label, `button small ${isInstalled ? 'ghost' : 'primary'}`, () => void this.install(hit));
        action.disabled = isInstalled || this.installing !== null;
        const title = Object.assign(document.createElement('a'), {
          href: projectPageUrl(hit.slug),
          target: '_blank',
          rel: 'noopener noreferrer',
          textContent: hit.title,
        });
        return item(hit.icon_url, [
          title,
          Object.assign(document.createElement('span'), { className: 'small', textContent: hit.description }),
          Object.assign(document.createElement('span'), {
            className: 'muted small',
            textContent: tr('modsHitMeta', downloads.format(hit.downloads), hit.author),
          }),
        ], action);
      }),
    );
  }
}

function button(label: string, className: string, onClick: () => void): HTMLButtonElement {
  const element = Object.assign(document.createElement('button'), { type: 'button', className, textContent: label });
  element.addEventListener('click', onClick);
  return element;
}

/** A row: icon, text lines, an action. */
function item(iconUrl: string | null, lines: HTMLElement[], action: HTMLElement): HTMLLIElement {
  const li = Object.assign(document.createElement('li'), { className: 'mod' });
  const icon = document.createElement(iconUrl ? 'img' : 'span');
  icon.className = 'mod-icon';
  icon.setAttribute('aria-hidden', 'true');
  if (icon instanceof HTMLImageElement && iconUrl) {
    // Cross-origin isolated page: the CDN's CORS headers make the image loadable.
    Object.assign(icon, { crossOrigin: 'anonymous', referrerPolicy: 'no-referrer', loading: 'lazy', alt: '', src: iconUrl });
    icon.addEventListener('error', () => icon.replaceWith(Object.assign(document.createElement('span'), { className: 'mod-icon' })), { once: true });
  }
  const text = Object.assign(document.createElement('div'), { className: 'mod-text' });
  text.append(...lines);
  li.append(icon, text, action);
  return li;
}
