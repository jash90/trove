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

  it('matches the initials of a multi-word name', () => {
    // The abbreviation a keyboard actually produces for a long name: a
    // launcher that cannot answer "vsc" reports "Visual Studio Code" as
    // unfindable even though it sits on the list.
    const spelled: AppEntry[] = [
      { name: 'Visual Studio Code', bundleId: null, path: '/Applications/Visual Studio Code.app' },
      { name: 'Vim', bundleId: null, path: '/Applications/Vim.app' },
    ];
    expect(filterApps(spelled, 'vsc').map((app) => app.name)).toEqual(['Visual Studio Code']);
    expect(filterApps(spelled, 'vs').map((app) => app.name)).toEqual(['Visual Studio Code']);
  });

  it('matches characters in order when nothing tighter fits', () => {
    // "chrm" is not inside "Chrome" — it is Chrome with the vowels left out.
    // "Chart" shares the start but never produces the m, so it stays out.
    const spelled: AppEntry[] = [
      { name: 'Google Chrome', bundleId: null, path: '/Applications/Google Chrome.app' },
      { name: 'Chrome', bundleId: null, path: '/Applications/Chrome.app' },
      { name: 'Chart', bundleId: null, path: '/Applications/Chart.app' },
    ];
    const results = filterApps(spelled, 'chrm').map((app) => app.name);
    expect(results).toContain('Google Chrome');
    expect(results).toContain('Chrome');
    expect(results).not.toContain('Chart');
  });

  it('treats non-alphanumeric separators as word boundaries', () => {
    const spelled: AppEntry[] = [
      { name: 'PDF-Expert', bundleId: null, path: '/Applications/PDF-Expert.app' },
      { name: 'Expert Witness', bundleId: null, path: '/Applications/Expert Witness.app' },
    ];
    const results = filterApps(spelled, 'expert').map((app) => app.name);
    expect(results).toEqual(['Expert Witness', 'PDF-Expert']);
  });

  it('finds an application by its bundle directory name', () => {
    // The plist's display name and the file on disk disagree: the user, who
    // knows the file, must still find the row.
    const spelled: AppEntry[] = [
      { name: 'Presentations', bundleId: 'com.example.studio', path: '/Applications/Visual Studio Code.app' },
      { name: 'Visual Novels', bundleId: null, path: '/Applications/Visual Novels.app' },
    ];
    const results = filterApps(spelled, 'visual studio code').map((app) => app.name);
    expect(results).toContain('Presentations');
    // The query is not prefixed to the displayed name, so the name ladder
    // stays outranked by a display name that actually begins with it.
    expect(filterApps(spelled, 'studio')).toEqual([
      { name: 'Presentations', bundleId: 'com.example.studio', path: '/Applications/Visual Studio Code.app' },
    ]);
  });

  it('ranks every rung of the ladder in order', () => {
    const ladder: AppEntry[] = [
      { name: 'Notebook Pro', bundleId: null, path: '/Applications/Notebook Pro.app' }, // name prefix
      { name: 'Zebra Notes', bundleId: null, path: '/Applications/Zebra Notes.app' }, // word prefix
      { name: 'Night Owl', bundleId: null, path: '/Applications/Night Owl.app' }, // initials
      { name: 'Kenote', bundleId: null, path: '/Applications/Kenote.app' }, // substring
      { name: 'Presentations', bundleId: null, path: '/Applications/Notes.app' }, // bundle directory
      { name: 'Renato', bundleId: null, path: '/Applications/Renato.app' }, // characters in order
      { name: 'Telegram', bundleId: null, path: '/Applications/Telegram.app' }, // nothing
    ];

    const results = filterApps(ladder, 'no').map((app) => app.name);

    expect(results).toEqual([
      'Notebook Pro',
      'Zebra Notes',
      'Night Owl',
      'Kenote',
      'Presentations',
      'Renato',
    ]);
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
