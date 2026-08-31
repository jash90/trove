import { useRef, type KeyboardEventHandler } from 'react';

export interface SettingsTab {
  id: string;
  label: string;
}

interface SettingsTabsProps {
  tabs: readonly SettingsTab[];
  active: string;
  onSelect: (id: string) => void;
}

/// The settings tab strip.
///
/// Follows the ARIA tabs pattern rather than a row of styled buttons: the strip is one stop in
/// the tab order and the arrows move between tabs. Six buttons that each take a Tab press would
/// put the whole settings dialog behind five presses of nothing.
///
/// Selection follows focus, which suits tabs whose panels are already rendered from local state —
/// there is no cost to arriving somewhere, so making someone press Enter to confirm arrival is a
/// ceremony that buys nothing.
export const SettingsTabs = ({
  tabs,
  active,
  onSelect,
}: SettingsTabsProps): React.JSX.Element => {
  const strip = useRef<HTMLDivElement>(null);

  const move = (to: number): void => {
    const target = tabs[(to + tabs.length) % tabs.length];
    if (!target) return;
    onSelect(target.id);
    // Focus follows the selection, or the arrows would move a highlight the keyboard has left
    // behind — and the next press would start from wherever focus actually was.
    strip.current
      ?.querySelector<HTMLButtonElement>(`[data-tab-id="${target.id}"]`)
      ?.focus();
  };

  const onKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    const index = tabs.findIndex((tab) => tab.id === active);
    if (index < 0) return;
    if (event.key === 'ArrowRight') {
      event.preventDefault();
      move(index + 1);
    } else if (event.key === 'ArrowLeft') {
      event.preventDefault();
      move(index - 1);
    } else if (event.key === 'Home') {
      event.preventDefault();
      move(0);
    } else if (event.key === 'End') {
      event.preventDefault();
      move(tabs.length - 1);
    }
  };

  return (
    <div className="settings-tabs" role="tablist" ref={strip} onKeyDown={onKeyDown}>
      {tabs.map((tab) => {
        const selected = tab.id === active;
        return (
          <button
            key={tab.id}
            type="button"
            role="tab"
            id={`settings-tab-${tab.id}`}
            data-tab-id={tab.id}
            aria-selected={selected}
            aria-controls={`settings-panel-${tab.id}`}
            // Only the selected tab is reachable by Tab; the arrows reach the rest. That is what
            // makes the strip one stop rather than six.
            tabIndex={selected ? 0 : -1}
            className={`settings-tabs__tab${selected ? ' is-selected' : ''}`}
            onClick={() => onSelect(tab.id)}
          >
            {tab.label}
          </button>
        );
      })}
    </div>
  );
};
