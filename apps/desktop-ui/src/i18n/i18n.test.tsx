import { act, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { EmptyState } from '../components/EmptyState';
import { en, type Message } from './en';
import {
  LOCALES,
  detectLocale,
  getLocale,
  setLocale,
  translate,
  type Locale,
} from './index';
import { pl } from './pl';

const placeholders = (message: Message): string[] => {
  const texts = typeof message === 'string' ? [message] : Object.values(message);
  return [...new Set(texts.flatMap((text) => text?.match(/\{\w+\}/gu) ?? []))].sort();
};

afterEach(() => {
  act(() => setLocale('en'));
});

describe('the translation tables', () => {
  it('have exactly the same keys in both languages', () => {
    expect(Object.keys(pl).sort()).toEqual(Object.keys(en).sort());
  });

  it('use the same placeholders in both languages', () => {
    for (const key of Object.keys(en) as (keyof typeof en)[]) {
      expect(placeholders(pl[key]), key).toEqual(placeholders(en[key]));
    }
  });

  it('give every plural entry the forms its language needs', () => {
    for (const [key, message] of Object.entries(en)) {
      if (typeof message === 'string') {
        expect(typeof pl[key as keyof typeof en], key).toBe('string');
        continue;
      }
      expect(message, key).toHaveProperty('one');
      expect(message, key).toHaveProperty('other');
      const polish = pl[key as keyof typeof en];
      expect(typeof polish, key).toBe('object');
      for (const form of ['one', 'few', 'many', 'other']) {
        expect(polish, `${key}.${form}`).toHaveProperty(form);
      }
    }
  });

  it('leave no empty strings', () => {
    for (const locale of Object.keys(LOCALES) as Locale[]) {
      for (const [key, message] of Object.entries(LOCALES[locale])) {
        const texts = typeof message === 'string' ? [message] : Object.values(message);
        for (const text of texts) expect(text?.trim(), `${locale}:${key}`).not.toBe('');
      }
    }
  });
});

describe('plurals', () => {
  it('follow the Polish one / few / many rules', () => {
    const records = (count: number) =>
      translate('pl', 'import.records', { count, n: String(count) });
    expect(records(1)).toBe('1 rekord');
    expect(records(2)).toBe('2 rekordy');
    expect(records(4)).toBe('4 rekordy');
    expect(records(5)).toBe('5 rekordów');
    expect(records(12)).toBe('12 rekordów');
    expect(records(22)).toBe('22 rekordy');
    expect(records(25)).toBe('25 rekordów');
  });

  it('follow the English one / other rules', () => {
    const records = (count: number) =>
      translate('en', 'import.records', { count, n: String(count) });
    expect(records(1)).toBe('1 record');
    expect(records(2)).toBe('2 records');
    expect(records(0)).toBe('0 records');
  });
});

describe('choosing the language', () => {
  it('is Polish only when Polish comes first among the languages it knows', () => {
    expect(detectLocale(['pl-PL', 'en-US'])).toBe('pl');
    expect(detectLocale(['pl'])).toBe('pl');
    expect(detectLocale(['de-DE', 'pl-PL'])).toBe('pl');
    expect(detectLocale(['en-GB', 'pl-PL'])).toBe('en');
    expect(detectLocale(['de-DE'])).toBe('en');
    expect(detectLocale([])).toBe('en');
    expect(detectLocale([undefined, null, ''])).toBe('en');
  });

  it('defaults to English in the test environment', () => {
    expect(getLocale()).toBe('en');
  });

  it('re-renders components when the language changes', () => {
    render(<EmptyState kind="empty" />);
    expect(screen.getByText('The history is empty')).toBeTruthy();
    act(() => setLocale('pl'));
    expect(screen.getByText('Historia jest pusta')).toBeTruthy();
  });

  it('fills placeholders', () => {
    expect(translate('pl', 'updates.latest', { version: '1.9.2' })).toBe(
      'Trove 1.9.2 to najnowsza wersja.',
    );
  });
});
