/// One byte range. Unsupported/malformed range syntax is ignored, never partially parsed.
#[derive(Debug, PartialEq)]
pub enum ByteRange {
    Full,
    Partial(usize, usize),
    Unsatisfiable,
}

pub fn parse_range(value: Option<&str>, size: usize) -> ByteRange {
    let Some(value) = value.and_then(|s| s.strip_prefix("bytes=")) else {
        return ByteRange::Full;
    };
    if value.contains(',') {
        return ByteRange::Full;
    }
    let Some((start, end)) = value.split_once('-') else {
        return ByteRange::Full;
    };
    if !start.bytes().all(|c| c.is_ascii_digit()) || !end.bytes().all(|c| c.is_ascii_digit()) {
        return ByteRange::Full;
    }
    if start.is_empty() {
        let Ok(suffix) = end.parse::<usize>() else {
            return ByteRange::Full;
        };
        if suffix == 0 || size == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Partial(size.saturating_sub(suffix), size - 1);
    }
    let Ok(start) = start.parse::<usize>() else {
        return ByteRange::Full;
    };
    if start >= size {
        return ByteRange::Unsatisfiable;
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        let Ok(end) = end.parse::<usize>() else {
            return ByteRange::Full;
        };
        if end < start {
            return ByteRange::Unsatisfiable;
        }
        end.min(size - 1)
    };
    ByteRange::Partial(start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_range() {
        assert_eq!(parse_range(None, 100), ByteRange::Full);
    }
    #[test]
    fn exact() {
        assert_eq!(
            parse_range(Some("bytes=10-19"), 100),
            ByteRange::Partial(10, 19)
        );
    }
    #[test]
    fn open_ended() {
        assert_eq!(
            parse_range(Some("bytes=10-"), 100),
            ByteRange::Partial(10, 99)
        );
    }
    #[test]
    fn suffix() {
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            ByteRange::Partial(90, 99)
        );
    }
    #[test]
    fn clamps() {
        assert_eq!(
            parse_range(Some("bytes=90-999"), 100),
            ByteRange::Partial(90, 99)
        );
        assert_eq!(
            parse_range(Some("bytes=-999"), 100),
            ByteRange::Partial(0, 99)
        );
    }
    #[test]
    fn invalid_bounds() {
        for s in ["bytes=100-", "bytes=20-10", "bytes=-0"] {
            assert_eq!(parse_range(Some(s), 100), ByteRange::Unsatisfiable);
        }
        assert_eq!(parse_range(Some("bytes=0-"), 0), ByteRange::Unsatisfiable);
    }
    #[test]
    fn ignores_unsupported() {
        for s in [
            "items=0-1",
            "bytes=0-1,5-6",
            "bytes=a-b",
            "bytes=-",
            "bytes=+1-2",
            "bytes=999999999999999999999999999-",
            "",
        ] {
            assert_eq!(parse_range(Some(s), 100), ByteRange::Full);
        }
    }
}
