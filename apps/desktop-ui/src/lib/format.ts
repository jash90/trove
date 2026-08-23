import type { ContentKind, Thumbnail } from './contracts';

const MAX_THUMBNAIL_BASE64_CHARACTERS = 360_000;
const BASE64_PATTERN = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

export const thumbnailDataUrl = (value: Thumbnail): string => {
  if (value.mimeType !== 'image/png') {
    throw new Error('thumbnail_invalid_mime');
  }
  if (value.base64.length > MAX_THUMBNAIL_BASE64_CHARACTERS) {
    throw new Error('thumbnail_too_large');
  }
  if (value.base64.length === 0 || !BASE64_PATTERN.test(value.base64)) {
    throw new Error('thumbnail_invalid_base64');
  }
  return `data:image/png;base64,${value.base64}`;
};

export const fileBasename = (value: string): string => {
  const segments = value.split(/[\\/]/u).filter(Boolean);
  return segments.at(-1) ?? 'Plik bez nazwy';
};

export const formatByteSize = (bytes: number): string => {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toLocaleString('pl-PL', { maximumFractionDigits: 1 })} KB`;
  return `${(bytes / 1_048_576).toLocaleString('pl-PL', { maximumFractionDigits: 1 })} MB`;
};

export const KIND_LABELS: Record<ContentKind, string> = {
  text: 'Tekst',
  link: 'Link',
  image: 'Obraz',
  file: 'Plik',
  color: 'Kolor',
  code: 'Kod',
  html: 'HTML',
};
