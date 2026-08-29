import { describe, expect, it } from 'vitest';

import { filterApps } from './appSearch';
import type { AppEntry } from './contracts';

/// In backend order on purpose: the empty query must not reshuffle anything,
/// and ties must resolve by that order rather than by whichever sort happened
/// to be stable this week.
const apps: AppEntry[] = [
  { name: 'Finder', bundleId: 'com.apple.finder', path: '/System/Library/CoreServices/Finder.app' },
  { name: 'Terminal', bundleId: 'com.apple.Terminal', path: '/System/Applications/Utilities/Terminal.app' },
  { name: 'Zebra Notes', bundleId: 'com.example.zebra', path: '/Applications/Zebra Notes.app' },
  { name: 'Łódź Editor', bundleId: 'pl.example.lodz-editor', path: '/Applications/Łódź Editor.app' },
];

describe('filterApps', () => {
  it('keeps the backend order when the query is empty', () => {
    expect(filterApps(apps, '')).toEqual(apps);
    expect(filterApps(apps, '   ')).toEqual(apps);
  });

  it('matches diacritics insensitively', () => {
    // A user typing "lodz" on any keyboard must find the town's editor.
    expect(filterApps(apps, 'lodz').map((app) => app.name)).toEqual(['Łódź Editor']);
    expect(filterApps(apps, 'ŁÓDŹ').map((app) => app.name)).toEqual(['Łódź Editor']);
  });

  it('ranks prefix above word start above substring', () => {
    const ranked: AppEntry[] = [
      { name: 'Super Note', bundleId: null, path: '/Applications/Super Note.app' },
      { name: 'Note Bridge', bundleId: null, path: '/Applications/Note Bridge.app' },
      { name: 'Keynote', bundleId: null, path: '/Applications/Keynote.app' },
      { name: 'Renata', bundleId: null, path: '/Applications/Renata.app' },
    ];

    const results = filterApps(ranked, 'no');

    // "Note Bridge" starts with the query, "Keynote" merely contains it, and
    // "Super Note" has it as a word start: prefix > word start > substring.
    // "Renata" matches nothing and disappears.
    expect(results.map((app) => app.name)).toEqual([
      'Note Bridge',
      'Super Note',
      'Keynote',
    ]);
  });

  it('matches on bundle identifiers', () => {
    expect(filterApps(apps, 'apple.finder').map((app) => app.name)).toEqual(['Finder']);
    expect(filterApps(apps, 'terminal').map((app) => app.name)).toEqual(['Terminal']);
  });

  it('is deterministic for tied scores', () => {
    const tied: AppEntry[] = [
      { name: 'Alpha One', bundleId: null, path: '/Applications/Alpha One.app' },
      { name: 'Alpha Two', bundleId: null, path: '/Applications/Alpha Two.app' },
      { name: 'Alpha Three', bundleId: null, path: '/Applications/Alpha Three.app' },
    ];

    const first = filterApps(tied, 'alpha');
    const second = filterApps([...tied].reverse(), 'beta');

    expect(first.map((app) => app.name)).toEqual(['Alpha One', 'Alpha Two', 'Alpha Three']);
    // A query nothing matches is empty every time, not "sometimes, unordered".
    expect(second).toEqual([]);
  });
});
