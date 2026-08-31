use serde::Deserialize;

use crate::CoreError;
use crate::error::ConfigParseError;

/// Deserializes a field, falling back to the type's default when the node is
/// absent. KDL-serde's deserializer only reports a clean `None` for omitted
/// fields when the target is `Option`, so this wraps and unwraps.
pub(crate) fn de_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn de_reserved<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(20_000))
}

pub(crate) fn de_tool_output_max_chars<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<usize, D::Error> {
    Ok(Option::<usize>::deserialize(deserializer)?.unwrap_or(16_000))
}

pub(crate) fn de_fallback_context<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(128_000))
}

pub(crate) fn de_frame_rate<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u32, D::Error> {
    Ok(Option::<u32>::deserialize(deserializer)?.unwrap_or(60))
}

pub(crate) fn from_str<T: serde::de::DeserializeOwned>(contents: &str) -> crate::Result<T> {
    kdl::de::from_str(contents).map_err(map_error)
}

pub(crate) fn to_string<T: serde::Serialize>(value: &T) -> std::result::Result<String, CoreError> {
    let mut doc = kdl::se::to_document(value).map_err(map_se_error)?;
    doc.autoformat();
    Ok(doc.to_string())
}

fn map_se_error(e: kdl::se::Error) -> CoreError {
    CoreError::ConfigParse(ConfigParseError {
        message: e.to_string(),
        line: 1,
        column: 1,
        length: 0,
        help: None,
    })
}

fn map_error(e: kdl::de::Error) -> CoreError {
    let diagnostic = e.diagnostic();
    let (line, column, length, message, help) = match &diagnostic {
        Some(d) => {
            let (line, column, length) =
                span_to_line_column(&d.input, d.span.offset(), d.span.len());
            (
                line,
                column,
                length,
                d.message.clone().unwrap_or_else(|| e.to_string()),
                d.help.clone(),
            )
        }
        None => (1, 1, 0, e.to_string(), None),
    };
    CoreError::ConfigParse(ConfigParseError {
        message,
        line,
        column,
        length,
        help,
    })
}

pub(crate) fn span_to_line_column(
    input: &str,
    offset: usize,
    length: usize,
) -> (usize, usize, usize) {
    let before = input.chars().take(offset).collect::<String>();
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map(|l| l.chars().count())
        .unwrap_or(0)
        + 1;
    (line, column, length)
}
