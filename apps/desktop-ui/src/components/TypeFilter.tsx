type FilterId = 'all' | 'text' | 'link' | 'image' | 'file' | 'pinned';

interface TypeFilterProps {
  query: string;
  onQueryChange: (query: string) => void;
}

interface FilterDefinition {
  id: FilterId;
  label: string;
  token: string | null;
}

const FILTERS: FilterDefinition[] = [
  { id: 'all', label: 'All', token: null },
  { id: 'text', label: 'Text', token: 'type:text' },
  { id: 'link', label: 'Links', token: 'type:link' },
  { id: 'image', label: 'Images', token: 'type:image' },
  { id: 'file', label: 'Files', token: 'type:file' },
  { id: 'pinned', label: 'Pinned', token: 'is:pinned' },
];

const FILTER_TOKEN_PATTERN = /(^|\s)(?:type:(?:text|link|image|file|color|code|html)|is:pinned)(?=\s|$)/giu;

const queryWithoutPaletteFilters = (query: string): string =>
  query.replace(FILTER_TOKEN_PATTERN, ' ').replace(/\s+/gu, ' ').trim();

/// Which filter the query currently expresses.
///
/// Returns nothing for an operator the palette has no control for — `type:code`
/// typed by hand is a real filter, and showing "All" as selected would
/// claim the opposite.
const activeFilterForQuery = (query: string): FilterId | null => {
  const match = query.match(/(?:^|\s)(type:(?:text|link|image|file|color|code|html)|is:pinned)(?=\s|$)/iu)?.[1];
  if (match === 'is:pinned') return 'pinned';
  if (match && /^type:(?:text|link|image|file)$/iu.test(match)) {
    return match.slice(5).toLocaleLowerCase('en-US') as FilterId;
  }
  if (match) return null;
  return 'all';
};

/// The type filter, as one control beside the search field.
///
/// It was a row of six chips, which is a row of chrome above every result for
/// a choice most searches never change. A palette is a search field and a list.
export const TypeFilter = ({
  query,
  onQueryChange,
}: TypeFilterProps): React.JSX.Element => {
  const activeFilter = activeFilterForQuery(query);

  return (
    <div className="type-filter">
      <label className="sr-only" htmlFor="history-type-filter">
        Type filter
      </label>
      <select
        id="history-type-filter"
        className="type-filter__select"
        value={activeFilter ?? ''}
        onChange={(event) => {
          const filter = FILTERS.find((candidate) => candidate.id === event.currentTarget.value);
          if (!filter) return;
          const baseQuery = queryWithoutPaletteFilters(query);
          onQueryChange([baseQuery, filter.token].filter(Boolean).join(' '));
        }}
      >
        {/* Only reachable when the query carries an operator with no control of
            its own; picking anything else replaces it. */}
        {activeFilter === null ? <option value="">Custom filter</option> : null}
        {FILTERS.map((filter) => (
          <option key={filter.id} value={filter.id}>
            {filter.label}
          </option>
        ))}
      </select>
    </div>
  );
};
