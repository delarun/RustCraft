// Shapes of Mojang's launcher metadata (piston-meta.mojang.com).

export type VersionType = 'release' | 'snapshot' | 'old_beta' | 'old_alpha';

export interface VersionManifest {
  latest: { release: string; snapshot: string };
  versions: VersionManifestEntry[];
}

export interface VersionManifestEntry {
  id: string;
  type: VersionType;
  url: string;
  sha1: string;
  time: string;
  releaseTime: string;
  complianceLevel: number;
}

export interface OsRule {
  name?: string;
  arch?: string;
  /** Regex matched against the OS version string. */
  version?: string;
  versionRange?: { min?: string; max?: string };
}

export interface Rule {
  action: 'allow' | 'disallow';
  os?: OsRule;
  features?: Record<string, boolean>;
}

export type Argument = string | { rules?: Rule[]; value: string | string[] };

export interface Artifact {
  path?: string;
  sha1: string;
  size: number;
  url: string;
}

export interface Library {
  /** Maven coordinate: group:artifact:version[:classifier] */
  name: string;
  downloads?: { artifact?: Artifact; classifiers?: Record<string, Artifact> };
  rules?: Rule[];
  /** Legacy (pre-1.19) native classifier mapping. */
  natives?: Record<string, string>;
}

export interface AssetIndexRef {
  id: string;
  sha1: string;
  size: number;
  totalSize: number;
  url: string;
}

export interface VersionJson {
  id: string;
  type: VersionType;
  mainClass: string;
  arguments?: {
    game: Argument[];
    jvm: Argument[];
    'default-user-jvm'?: Argument[];
  };
  /** Legacy (pre-1.13) space-separated game arguments. */
  minecraftArguments?: string;
  assetIndex: AssetIndexRef;
  assets: string;
  downloads: { client: Artifact; server?: Artifact } & Record<string, Artifact>;
  javaVersion?: { component: string; majorVersion: number };
  libraries: Library[];
  logging?: {
    client?: {
      argument: string;
      file: { id: string; sha1: string; size: number; url: string };
      type: string;
    };
  };
}

export interface AssetIndex {
  objects: Record<string, { hash: string; size: number }>;
  virtual?: boolean;
  map_to_resources?: boolean;
}
