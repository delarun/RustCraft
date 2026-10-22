export interface MavenCoordinate {
  group: string;
  artifact: string;
  version: string;
  classifier?: string;
}

export function parseMaven(name: string): MavenCoordinate {
  const [group, artifact, version, classifier] = name.split(':');
  if (!group || !artifact || !version) throw new Error(`Bad maven coordinate: ${name}`);
  return { group, artifact, version, classifier };
}

export function mavenPath({ group, artifact, version, classifier }: MavenCoordinate): string {
  const file = `${artifact}-${version}${classifier ? `-${classifier}` : ''}.jar`;
  return `${group.replaceAll('.', '/')}/${artifact}/${version}/${file}`;
}

/**
 * Host-native jars (`natives-linux`, Netty's `linux-x86_64`, ...). Their .so/.dll
 * payloads are useless in the browser — our JVM supplies WASM implementations.
 */
export function isHostNativeClassifier(classifier: string | undefined): boolean {
  return classifier !== undefined && /^(natives-|(linux|osx|windows|macos)-)/.test(classifier);
}
