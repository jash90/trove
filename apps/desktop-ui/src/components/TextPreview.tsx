import { useT } from '../i18n';
import type { Preview } from '../lib/contracts';

interface TextPreviewProps {
  preview: Preview;
}

export const TextPreview = ({ preview }: TextPreviewProps): React.JSX.Element => {
  const t = useT();
  const content = preview.text ?? t('text.empty');

  if (preview.kind === 'file') {
    return (
      <div className="text-preview text-preview--file">
        <span className="preview-label">{t('text.filePath')}</span>
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
