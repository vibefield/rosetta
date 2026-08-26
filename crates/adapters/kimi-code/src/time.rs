//! Kimi persists timestamps as unsigned unix milliseconds.

use rosetta_core::rows::Ms;

pub fn epoch_ms(value: u64) -> Option<Ms> {
    i64::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::epoch_ms;

    #[test]
    fn converts_representable_values_only() {
        assert_eq!(epoch_ms(0), Some(0));
        assert_eq!(epoch_ms(1_787_636_361_887), Some(1_787_636_361_887));
        assert_eq!(epoch_ms(u64::MAX), None);
    }
}
