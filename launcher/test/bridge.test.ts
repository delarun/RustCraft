import { describe, expect, it } from 'vitest';
import { BRIDGE_DOWNLOAD_URL, bridgeAddress, DEFAULT_BRIDGE, parseBridgeAddress } from '../src/net/bridge';

describe('bridge address', () => {
  it('accepts ws:// and wss:// addresses, normalized', () => {
    expect(parseBridgeAddress('ws://127.0.0.1:25580')).toBe('ws://127.0.0.1:25580');
    expect(parseBridgeAddress('  ws://127.0.0.1:25580/ ')).toBe('ws://127.0.0.1:25580');
    expect(parseBridgeAddress('WS://LOCALHOST:25580')).toBe('ws://localhost:25580');
    expect(parseBridgeAddress('wss://bridge.example.org')).toBe('wss://bridge.example.org');
    expect(parseBridgeAddress('ws://[::1]:25580')).toBe('ws://[::1]:25580');
  });

  it('refuses anything else', () => {
    for (const value of [
      '',
      '127.0.0.1:25580',
      'http://127.0.0.1:25580',
      'https://127.0.0.1:25580',
      'ws://',
      'ws:/127.0.0.1',
      'ws://127.0.0.1:25580/connect',
      'ws://127.0.0.1:25580?x=1',
      'ws://127.0.0.1:25580#x',
      'ws://user:pass@127.0.0.1:25580',
      'ws://127.0.0.1:99999',
      'javascript:alert(1)',
    ]) {
      expect(parseBridgeAddress(value), value).toBeNull();
    }
  });

  it('falls back to the default bridge', () => {
    expect(DEFAULT_BRIDGE).toBe('ws://127.0.0.1:25580');
    expect(bridgeAddress(null)).toBe(DEFAULT_BRIDGE);
    expect(bridgeAddress('')).toBe(DEFAULT_BRIDGE);
    expect(bridgeAddress('http://nope')).toBe(DEFAULT_BRIDGE);
    expect(bridgeAddress('ws://192.168.1.5:25580/')).toBe('ws://192.168.1.5:25580');
  });

  it('links to the release downloads', () => {
    expect(BRIDGE_DOWNLOAD_URL).toBe('https://github.com/delarun/RustCraft/releases/latest');
  });
});
