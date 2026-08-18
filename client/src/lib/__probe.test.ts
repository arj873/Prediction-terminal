import { describe, it, expect } from 'vitest';
import { Probe } from './__probe.svelte.js';
describe('probe', () => { it('works', () => { const p = new Probe(); p.inc(); expect(p.n).toBe(1); }); });
