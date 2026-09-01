use thiserror::Error;
use trove_core::{ContentKind, normalize_search_text};

pub const MAX_RAW_QUERY_BYTES: usize = 8 * 1024;
pub const MAX_APP_FILTER_BYTES: usize = 512;
pub const MAX_SEARCH_TERMS: usize = 32;
pub const MAX_SEARCH_TERM_BYTES: usize = 240;
pub const MAX_FTS_MATCH_BYTES: usize = 8 * 1024;

pub struct SearchFilters {
    pub kind: Option<ContentKind>,
    pub app: Option<String>,
    pub pinned: Option<bool>,
}

pub struct ParsedQuery {
    pub text: String,
    pub filters: SearchFilters,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum QueryError {
    #[error("query_too_long")]
    QueryTooLong,
    #[error("app_filter_too_long")]
    AppFilterTooLong,
    #[error("too_many_search_terms")]
    TooManySearchTerms,
    #[error("search_term_too_long")]
    SearchTermTooLong,
    #[error("query_too_long")]
    MatchExpressionTooLong,
    #[error("invalid type filter")]
    InvalidTypeFilter,
    #[error("duplicate type filter")]
    DuplicateTypeFilter,
    #[error("invalid app filter")]
    InvalidAppFilter,
    #[error("duplicate app filter")]
    DuplicateAppFilter,
    #[error("invalid is filter")]
    InvalidIsFilter,
    #[error("duplicate is filter")]
    DuplicateIsFilter,
}

impl QueryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::QueryTooLong | Self::MatchExpressionTooLong => "query_too_long",
            Self::AppFilterTooLong => "app_filter_too_long",
            Self::TooManySearchTerms => "too_many_search_terms",
            Self::SearchTermTooLong => "search_term_too_long",
            Self::InvalidTypeFilter => "invalid_type_filter",
            Self::DuplicateTypeFilter => "duplicate_type_filter",
            Self::InvalidAppFilter => "invalid_app_filter",
            Self::DuplicateAppFilter => "duplicate_app_filter",
            Self::InvalidIsFilter => "invalid_is_filter",
            Self::DuplicateIsFilter => "duplicate_is_filter",
        }
    }
}

pub fn parse_query(query: &str) -> Result<ParsedQuery, QueryError> {
    if query.len() > MAX_RAW_QUERY_BYTES {
        return Err(QueryError::QueryTooLong);
    }
    let mut filters = SearchFilters {
        kind: None,
        app: None,
        pinned: None,
    };
    let mut residual = Vec::new();
    let mut offset = 0;

    while offset < query.len() {
        offset = skip_whitespace(query, offset);
        if offset == query.len() {
            break;
        }
        let start = offset;
        if query[start..].starts_with("app:") {
            let (value, next) = parse_app_value(query, start)?;
            if filters.app.is_some() {
                return Err(QueryError::DuplicateAppFilter);
            }
            filters.app = Some(value);
            offset = next;
            continue;
        }

        let end = token_end(query, start);
        let token = &query[start..end];
        if let Some(value) = token.strip_prefix("type:") {
            if filters.kind.is_some() {
                return Err(QueryError::DuplicateTypeFilter);
            }
            filters.kind = Some(parse_kind(value).ok_or(QueryError::InvalidTypeFilter)?);
        } else if let Some(value) = token.strip_prefix("is:") {
            if filters.pinned.is_some() {
                return Err(QueryError::DuplicateIsFilter);
            }
            if value != "pinned" {
                return Err(QueryError::InvalidIsFilter);
            }
            filters.pinned = Some(true);
        } else {
            residual.push(token);
        }
        offset = end;
    }

    Ok(ParsedQuery {
        text: normalize_search_text(&residual.join(" ")),
        filters,
    })
}

/// Builds the FTS5 `MATCH` expression for a query.
///
/// The final term is matched as a prefix. A palette is typed into one character
/// at a time, and a whole-word match shows nothing until the word is finished —
/// so a search for `supercm` found nothing while `supercmd` found eight
/// entries. Earlier terms stay exact: the user finished typing those.
pub fn build_fts_match_expression(normalized_text: &str) -> Result<String, QueryError> {
    let mut expression = String::with_capacity(normalized_text.len().min(MAX_FTS_MATCH_BYTES));
    let mut term_count = 0_usize;
    let mut terms = normalized_text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .peekable();
    while let Some(term) = terms.next() {
        if term.len() > MAX_SEARCH_TERM_BYTES {
            return Err(QueryError::SearchTermTooLong);
        }
        term_count += 1;
        if term_count > MAX_SEARCH_TERMS {
            return Err(QueryError::TooManySearchTerms);
        }
        let is_last = terms.peek().is_none();
        let separator_bytes = if expression.is_empty() {
            0
        } else {
            " AND ".len()
        };
        let quoting_bytes = if is_last { 3 } else { 2 };
        let required = expression
            .len()
            .checked_add(separator_bytes)
            .and_then(|length| length.checked_add(term.len()))
            .and_then(|length| length.checked_add(quoting_bytes))
            .ok_or(QueryError::MatchExpressionTooLong)?;
        if required > MAX_FTS_MATCH_BYTES {
            return Err(QueryError::MatchExpressionTooLong);
        }
        if separator_bytes != 0 {
            expression.push_str(" AND ");
        }
        expression.push('"');
        expression.push_str(term);
        expression.push('"');
        if is_last {
            expression.push('*');
        }
    }
    Ok(expression)
}

fn skip_whitespace(value: &str, mut offset: usize) -> usize {
    while let Some(character) = value[offset..].chars().next() {
        if !character.is_whitespace() {
            break;
        }
        offset += character.len_utf8();
    }
    offset
}

fn token_end(value: &str, mut offset: usize) -> usize {
    while let Some(character) = value[offset..].chars().next() {
        if character.is_whitespace() {
            break;
        }
        offset += character.len_utf8();
    }
    offset
}

fn parse_app_value(query: &str, start: usize) -> Result<(String, usize), QueryError> {
    let value_start = start + "app:".len();
    if value_start == query.len() {
        return Err(QueryError::InvalidAppFilter);
    }
    if query[value_start..].starts_with('"') {
        let content_start = value_start + 1;
        let Some(relative_end) = query[content_start..].find('"') else {
            return Err(QueryError::InvalidAppFilter);
        };
        let content_end = content_start + relative_end;
        let next = content_end + 1;
        if query[content_start..content_end].trim().is_empty()
            || query[next..]
                .chars()
                .next()
                .is_some_and(|character| !character.is_whitespace())
        {
            return Err(QueryError::InvalidAppFilter);
        }
        if content_end - content_start > MAX_APP_FILTER_BYTES {
            return Err(QueryError::AppFilterTooLong);
        }
        return Ok((query[content_start..content_end].to_owned(), next));
    }

    let end = token_end(query, value_start);
    if end == value_start {
        return Err(QueryError::InvalidAppFilter);
    }
    if end - value_start > MAX_APP_FILTER_BYTES {
        return Err(QueryError::AppFilterTooLong);
    }
    Ok((query[value_start..end].to_owned(), end))
}

fn parse_kind(value: &str) -> Option<ContentKind> {
    match value {
        "text" => Some(ContentKind::Text),
        "link" => Some(ContentKind::Link),
        "image" => Some(ContentKind::Image),
        "file" => Some(ContentKind::File),
        "color" => Some(ContentKind::Color),
        "code" => Some(ContentKind::Code),
        "html" => Some(ContentKind::Html),
        _ => None,
    }
}
