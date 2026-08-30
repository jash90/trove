import { KeyRound } from 'lucide-react';
import type { CSSProperties, MouseEventHandler } from 'react';

import type { KeyvaultSecret } from '../lib/contracts';

interface VaultRowProps {
  secret: KeyvaultSecret;
  /** Position in the merged list; the option id is announced from it. */
  index: number;
  selected: boolean;
  style: CSSProperties;
  onSelect: () => void;
  onActivate: () => void;
}

/// One secret from the vault, offered by name.
///
/// Shows the slug, the display name and the category — never the value. That line is the same
/// one the settings pane holds: a secret's value exists only between the decrypt and the
/// clipboard write, and putting it in a list would be exactly the leak the vault channel is
/// built to avoid. Activating a row copies through the core, which arms the capture suppression
/// first, so a fetched key does not land in the history either.
///
/// Deliberately the same row rhythm as an application and a history entry, so all three sit in
/// one palette as siblings rather than three interfaces stitched together.
export const VaultRow = ({
  secret,
  index,
  selected,
  style,
  onSelect,
  onActivate,
}: VaultRowProps): React.JSX.Element => {
  const handleClick: MouseEventHandler<HTMLDivElement> = () => onSelect();
  const handleDoubleClick: MouseEventHandler<HTMLDivElement> = () => onActivate();

  return (
    <div
      id={`vault-option-${index}`}
      role="option"
      aria-selected={selected}
      aria-label={`Vault secret: ${secret.name}`}
      data-slug={secret.slug}
      className={`history-row vault-row${selected ? ' is-selected' : ''}`}
      style={style}
      onClick={handleClick}
      onDoubleClick={handleDoubleClick}
    >
      <span className="history-row__kind" aria-hidden="true">
        <KeyRound size={17} strokeWidth={1.8} />
      </span>
      <span className="history-row__content">
        <span className="history-row__preview">{secret.name}</span>
        <span className="history-row__metadata">
          <span>{secret.slug}</span>
          {secret.category ? <span>{secret.category}</span> : null}
          <span>Keyvault</span>
        </span>
      </span>
    </div>
  );
};
