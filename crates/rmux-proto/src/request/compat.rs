use serde::de::{self, SeqAccess, Visitor};
use serde::Deserialize;

pub(in crate::request) fn required_next<'de, A, T, V>(
    seq: &mut A,
    index: usize,
    visitor: &V,
) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de>,
    V: Visitor<'de>,
{
    seq.next_element()?
        .ok_or_else(|| de::Error::invalid_length(index, visitor))
}

pub(in crate::request) fn compat_next_element<'de, A, T>(seq: &mut A) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de> + Default,
{
    match seq.next_element::<T>() {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Ok(T::default()),
        Err(error) if is_truncated_compat_sequence(&error) => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn is_truncated_compat_sequence(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string();
    message.contains("UnexpectedEof") || message.contains("unexpected end of file")
}
