/**
 * The interface's two languages: English, which is the default and the
 * fallback for any key a translation lacks, and Polish, chosen when the
 * system's preferred language is Polish.
 *
 * Deliberately small: one flat table per language, `{name}` placeholders and
 * `Intl.PluralRules` for counts. The locale is module state rather than React
 * context so plain helpers (formatters, error messages) can translate too;
 * components read it through `useT`, which re-renders them when it changes.
 */
import { useSyncExternalStore } from 'react';

import { en, type MessageKey, type Messages, type PluralForms } from './en';
import { pl } from './pl';

export type Locale = 'en' | 'pl';
export type { MessageKey, Messages, PluralForms };

export const LOCALES: Record<Locale, Messages> = { en, pl };

/** Polish when the first preferred language that we can tell apart is Polish. */
export const detectLocale = (languages: readonly (string | undefined | null)[]): Locale => {
  for (const language of languages) {
    if (!language) continue;
    const tag = language.toLowerCase();
    if (tag === 'pl' || tag.startsWith('pl-') || tag.startsWith('pl_')) return 'pl';
    if (tag === 'en' || tag.startsWith('en-') || tag.startsWith('en_')) return 'en';
  }
  return 'en';
};

const navigatorLanguages = (): string[] => {
  if (typeof navigator === 'undefined') return [];
  const list = navigator.languages?.length ? [...navigator.languages] : [];
  return navigator.language ? [...list, navigator.language] : list;
};

let current: Locale = detectLocale(navigatorLanguages());
const listeners = new Set<() => void>();

export const getLocale = (): Locale => current;

export const setLocale = (locale: Locale): void => {
  if (locale === current) return;
  current = locale;
  if (typeof document !== 'undefined') document.documentElement.lang = locale;
  for (const listener of listeners) listener();
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

export type Params = Record<string, string | number>;

const interpolate = (template: string, params?: Params): string =>
  params
    ? template.replace(/\{(\w+)\}/gu, (match, name: string) =>
        name in params ? String(params[name]) : match,
      )
    : template;

const pluralRules = new Map<Locale, Intl.PluralRules>();
const rulesFor = (locale: Locale) => {
  let rules = pluralRules.get(locale);
  if (!rules) {
    rules = new Intl.PluralRules(locale);
    pluralRules.set(locale, rules);
  }
  return rules;
};

const pick = (forms: PluralForms, locale: Locale, count: number): string => {
  const category = rulesFor(locale).select(count) as keyof PluralForms;
  return forms[category] ?? forms.other;
};

/** Translates `key` into `locale`; a plural entry takes its form from `params.count`. */
export const translate = (locale: Locale, key: MessageKey, params?: Params): string => {
  const entry = LOCALES[locale][key] ?? en[key];
  if (typeof entry === 'string') return interpolate(entry, params);
  const count = Number(params?.count ?? 0);
  return interpolate(pick(entry, locale, count), params);
};

/** Translates into the current locale. */
export const t = (key: MessageKey, params?: Params): string => translate(current, key, params);

export type Translate = typeof t;

/** The current locale, re-rendering the caller when it changes. */
export const useLocale = (): Locale => useSyncExternalStore(subscribe, getLocale, getLocale);

/// One bound function per locale, so it is stable across renders and safe in
/// hook dependency lists.
const bound: Record<Locale, Translate> = {
  en: (key, params) => translate('en', key, params),
  pl: (key, params) => translate('pl', key, params),
};

/** A `t` bound to the current locale; components re-render on a change. */
export const useT = (): Translate => bound[useLocale()];
