import { describe, expect, it } from 'vitest';

import type { AppEntry, HistoryItem } from './contracts';
import { buildPaletteItems, keyOfItem } from './paletteItems';

const app = (path: string): AppEntry => ({ name: path, bundleId: null, path });

const history = (eventId: number): HistoryItem => ({
  eventId,
  globalId: `0198f000-0000-7000-8000-${String(eventId).padStart(12, '0')}`,
  kind: 'text',
  capturedAtMs: 1_775_000_000_000 - eventId,
  sourceAppName: 'Synthetic Editor',
  pinned: false,
  preview: `Synthetic item ${eventId}`,
  byteSize: 32,
  hasThumbnail: false,
  occurrenceCount: 1,
  occurrences: [1_775_000_000_000 - eventId],
});

const kindsOf = (items: readonly { kind: string }[]): string[] =>
  items.map((item) => item.kind);

describe('buildPaletteItems', () => {
  it('with no query, applications come first and the history follows', () => {
    const items = buildPaletteItems([app('/a'), app('/b')], [history(1), history(2)], '');

    expect(kindsOf(items)).toEqual(['app', 'app', 'history', 'history']);
    expect(items[0]?.kind === 'app' && items[0].app.path).toBe('/a');
  });

  it('a whitespace-only query counts as no query', () => {
    expect(kindsOf(buildPaletteItems([app('/a')], [history(1)], '  '))).toEqual([
      'app',
      'history',
    ]);
  });

  it('a typed query interleaves the two ranked lists, applications first at each depth', () => {
    const items = buildPaletteItems(
      [app('/a1'), app('/a2'), app('/a3')],
      [history(1), history(2)],
      'note',
    );

    expect(kindsOf(items)).toEqual(['app', 'history', 'app', 'history', 'app']);
  });

  it('unequal lengths append the remainder in that list\'s own rank order', () => {
    const items = buildPaletteItems([app('/only')], [history(1), history(2), history(3)], 'q');

    expect(kindsOf(items)).toEqual(['app', 'history', 'history', 'history']);
  });

  it('either side may be empty', () => {
    expect(kindsOf(buildPaletteItems([], [history(1)], ''))).toEqual(['history']);
    expect(kindsOf(buildPaletteItems([app('/a')], [], 'q'))).toEqual(['app']);
    expect(buildPaletteItems([], [], 'q')).toEqual([]);
  });

  it('keys are unique and stable across rebuilds', () => {
    const first = buildPaletteItems([app('/a')], [history(7), history(9)], 'q');
    const second = buildPaletteItems([app('/a')], [history(7), history(9)], 'q');

    expect(first.map(keyOfItem)).toEqual(['/a', 'h7', 'h9']);
    expect(second.map(keyOfItem)).toEqual(first.map(keyOfItem));
    expect(new Set(first.map(keyOfItem)).size).toBe(first.length);
  });
});
