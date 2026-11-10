#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct S3Location {
    pub(crate) bucket: String,
    pub(crate) key: String,
}

impl S3Location {
    pub(crate) fn prefix(location: &str) -> Option<String> {
        let prefix = if location.ends_with('/') {
            location.to_owned()
        } else {
            format!("{location}/")
        };
        Self::parse(&prefix).map(|_| prefix)
    }

    pub(crate) fn parse(location: &str) -> Option<Self> {
        let (bucket, key) = location.strip_prefix("s3://")?.split_once('/')?;
        if !(3..=63).contains(&bucket.len())
            || !bucket
                .bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || b".-".contains(&ch))
            || !bucket.as_bytes().first()?.is_ascii_alphanumeric()
            || !bucket.as_bytes().last()?.is_ascii_alphanumeric()
            || key
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '?' | '#' | '\\' | '%'))
            || key.split('/').any(|part| matches!(part, "." | ".."))
        {
            return None;
        }
        Some(Self {
            bucket: bucket.to_owned(),
            key: key.to_owned(),
        })
    }
}
