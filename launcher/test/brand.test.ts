import { describe, expect, it } from 'vitest';
import html from '../index.html?raw';
import { DISCLAIMER, PRODUCT_NAME, SITE_URL, SOURCE_URL } from '../src/brand';

describe('branding', () => {
  it('keeps the static page title and header equal to the constant', () => {
    expect(html).toContain(`<title>${PRODUCT_NAME}</title>`);
    expect(html).toContain(`<h1 id="product-name">${PRODUCT_NAME}</h1>`);
    expect(html).toContain(`<meta property="og:title" content="${PRODUCT_NAME}" />`);
  });

  it('points canonical metadata at the public site', () => {
    expect(SITE_URL).toBe('https://rustcraft.online');
    expect(html).toContain(`<link rel="canonical" href="${SITE_URL}/" />`);
    expect(html).toContain(`<meta property="og:url" content="${SITE_URL}/" />`);
  });

  it('shows the disclaimer and uses no images', () => {
    expect(html).toContain(DISCLAIMER);
    expect(html).not.toMatch(/<img|<svg|\.png|\.jpg|\.webp/i);
  });

  it('links the source code (AGPL) and the third-party notices from the footer', () => {
    expect(html).toContain(`<a id="source-link" href="${SOURCE_URL}"`);
    expect(html).toContain('href="third-party-notices.txt"');
  });
});
