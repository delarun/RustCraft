// On-disk layout of the game directory. The same relative paths are used in OPFS
// (under OPFS_ROOT) and inside the JVM's filesystem (under GAME_DIR), mirroring
// a regular .minecraft directory. Saves, options and logs written by the game
// land in the same directory, so they persist in OPFS.

export const OPFS_ROOT = 'minecraft';
export const GAME_DIR = '/minecraft';

export const layout = {
  versionJson: (id: string) => `versions/${id}/${id}.json`,
  clientJar: (id: string) => `versions/${id}/${id}.jar`,
  library: (path: string) => `libraries/${path}`,
  assetIndex: (id: string) => `assets/indexes/${id}.json`,
  assetObject: (hash: string) => `assets/objects/${hash.slice(0, 2)}/${hash}`,
  logConfig: (id: string) => `assets/log_configs/${id}`,
  runtimeManifest: (component: string) => `runtime/${component}.json`,
  runtimeFile: (component: string, path: string) => `runtime/${component}/${path}`,
  /** Fabric Loader loads every jar here (the game sees it as /minecraft/mods). */
  mods: 'mods',
  mod: (file: string) => `mods/${file}`,
  /** What the launcher knows about the mods it installed from Modrinth (Fabric reads only *.jar). */
  modsManifest: 'mods/rustcraft-mods.json',
};

/** Top-level directories that hold downloads (everything else is the player's: saves, options, ...). */
export const DOWNLOAD_DIRS = ['versions', 'libraries', 'assets', 'runtime'];

export const inGameDir = (relative: string) => `${GAME_DIR}/${relative}`;
