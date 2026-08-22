use clipboard_core::{ContentKind, normalize_search_text};
use thiserror::Error;

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

pub(crate) fn fts_match_expression(normalized_text: &str) -> String {
    normalized_text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" AND ")
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
        return Ok((query[content_start..content_end].to_owned(), next));
    }

    let end = token_end(query, value_start);
    if end == value_start {
        return Err(QueryError::InvalidAppFilter);
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
