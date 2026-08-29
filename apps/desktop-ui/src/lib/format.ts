import { MAX_THUMBNAIL_BASE64_BYTES, type ContentKind, type Thumbnail } from './contracts';

const BASE64_PATTERN = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

export const thumbnailDataUrl = (value: Thumbnail): string => {
  if (value.mimeType !== 'image/png') {
    throw new Error('thumbnail_invalid_mime');
  }
  if (value.base64.length > MAX_THUMBNAIL_BASE64_BYTES) {
    throw new Error('thumbnail_too_large');
  }
  if (value.base64.length === 0 || !BASE64_PATTERN.test(value.base64)) {
    throw new Error('thumbnail_invalid_base64');
  }
  return `data:image/png;base64,${value.base64}`;
};

/**
 * Grouped here rather than through `toLocaleString`, which varies with the
 * host's ICU version and can emit non-breaking separators — those read badly
 * and are awkward to assert on. Every three digits, one comma, always.
 */
export const formatCount = (value: number): string =>
  String(value).replace(/\B(?=(?:\d{3})+(?!\d))/gu, ',');

export const fileBasename = (value: string): string => {
  const segments = value.split(/[\\/]/u).filter(Boolean);
  return segments.at(-1) ?? 'Unnamed file';
};

export const formatByteSize = (bytes: number): string => {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toLocaleString('en-US', { maximumFractionDigits: 1 })} KB`;
  return `${(bytes / 1_048_576).toLocaleString('en-US', { maximumFractionDigits: 1 })} MB`;
};

export const KIND_LABELS: Record<ContentKind, string> = {
  text: 'Text',
  link: 'Link',
  image: 'Image',
  file: 'File',
  color: 'Colour',
  code: 'Code',
  html: 'HTML',
};

const CAPTURED_AT_FORMATTER = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
});

export const formatCapturedAt = (capturedAtMs: number): string =>
  CAPTURED_AT_FORMATTER.format(capturedAtMs);
