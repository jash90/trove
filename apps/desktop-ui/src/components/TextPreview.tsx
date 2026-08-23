import type { Preview } from '../lib/contracts';

interface TextPreviewProps {
  preview: Preview;
}

export const TextPreview = ({ preview }: TextPreviewProps): React.JSX.Element => {
  const content = preview.text ?? 'Brak treści do wyświetlenia';

  if (preview.kind === 'file') {
    return (
      <div className="text-preview text-preview--file">
        <span className="preview-label">Ścieżka pliku</span>
        <p>{content}</p>
      </div>
    );
  }

  return (
    <pre className={`text-preview text-preview--${preview.kind}`} tabIndex={0}>
      {content}
    </pre>
  );
};
