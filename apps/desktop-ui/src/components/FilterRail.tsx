import { File, Image, Link2, ListFilter, Pin, Type, type LucideIcon } from 'lucide-react';

type FilterId = 'all' | 'text' | 'link' | 'image' | 'file' | 'pinned';

interface FilterRailProps {
  query: string;
  onQueryChange: (query: string) => void;
}

interface FilterDefinition {
  id: FilterId;
  label: string;
  token: string | null;
  icon: LucideIcon;
}

const FILTERS: FilterDefinition[] = [
  { id: 'all', label: 'Wszystkie', token: null, icon: ListFilter },
  { id: 'text', label: 'Tekst', token: 'type:text', icon: Type },
  { id: 'link', label: 'Linki', token: 'type:link', icon: Link2 },
  { id: 'image', label: 'Obrazy', token: 'type:image', icon: Image },
  { id: 'file', label: 'Pliki', token: 'type:file', icon: File },
  { id: 'pinned', label: 'Przypięte', token: 'is:pinned', icon: Pin },
];

const FILTER_TOKEN_PATTERN = /(^|\s)(?:type:(?:text|link|image|file|color|code|html)|is:pinned)(?=\s|$)/giu;

const queryWithoutPaletteFilters = (query: string): string =>
  query.replace(FILTER_TOKEN_PATTERN, ' ').replace(/\s+/gu, ' ').trim();

const activeFilterForQuery = (query: string): FilterId | null => {
  const match = query.match(/(?:^|\s)(type:(?:text|link|image|file|color|code|html)|is:pinned)(?=\s|$)/iu)?.[1];
  if (match === 'is:pinned') return 'pinned';
  if (match && /^type:(?:text|link|image|file)$/iu.test(match)) {
    return match.slice(5).toLocaleLowerCase('en-US') as FilterId;
  }
  if (match) return null;
  return 'all';
};

export const FilterRail = ({
  query,
  onQueryChange,
}: FilterRailProps): React.JSX.Element => {
  const activeFilter = activeFilterForQuery(query);

  return (
    <nav className="filter-rail" aria-label="Filtry historii">
      <span className="filter-rail__label">Widok</span>
      <div className="filter-rail__options">
        {FILTERS.map((filter) => {
          const Icon = filter.icon;
          const handleClick = (): void => {
            const baseQuery = queryWithoutPaletteFilters(query);
            onQueryChange([baseQuery, filter.token].filter(Boolean).join(' '));
          };

          return (
            <button
              key={filter.id}
              type="button"
              className="filter-chip"
              aria-pressed={activeFilter === filter.id}
              onClick={handleClick}
            >
              <Icon size={14} strokeWidth={1.8} aria-hidden="true" />
              {filter.label}
            </button>
          );
        })}
      </div>
    </nav>
  );
};
