/**
 * The two things about the workspace that need a browser to be true: it paints
 * the theme onto the document, and left to itself it uses the real
 * `localStorage`. Everything else about it is tested in `workspace.test.ts`,
 * under Node, where most of this client's tests live.
 */

import { beforeEach, describe, expect, it } from 'vitest';
import { Workspace } from './workspace.svelte';

const STORAGE_KEY = 'prediction-terminal:v1';

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset['theme'];
});

describe('Workspace in a document', () => {
  it('paints the theme where the stylesheet can see it', () => {
    const workspace = new Workspace();

    workspace.setTheme('ice');
    expect(document.documentElement.dataset['theme']).toBe('ice');

    // And again on demand, which is what boot does before the first paint.
    delete document.documentElement.dataset['theme'];
    workspace.applyTheme();
    expect(document.documentElement.dataset['theme']).toBe('ice');
  });

  it('persists to the storage the browser gives it, under the key it always used', () => {
    const workspace = new Workspace();
    workspace.addToWatchlist('KXFED-1');
    workspace.setColumns(4);

    expect(localStorage.getItem(STORAGE_KEY)).toBe(
      '{"watchlist":["KXFED-1"],"history":[],"theme":"amber","columns":4,"keys":{}}',
    );
    expect(new Workspace().watchlist).toEqual(['KXFED-1']);
  });
});
