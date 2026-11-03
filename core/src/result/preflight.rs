//! Bounded validation before Parquet metadata and Arrow decoding.
//! The Compact Protocol is specified by Apache Thrift; Parquet page headers
//! carry their compressed and uncompressed sizes independently of the footer.
use parquet::file::metadata::ParquetMetaData;

use super::{ResultError, ResultLimits};
use crate::query::RequestGuard;

const MAX_HEADER: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;

fn malformed() -> ResultError {
    ResultError::InvalidParquet("invalid compact metadata or page framing")
}

pub(super) fn footer(bytes: &[u8]) -> Result<usize, ResultError> {
    if bytes.len() < 12 || &bytes[..4] != b"PAR1" || &bytes[bytes.len() - 4..] != b"PAR1" {
        return Err(malformed());
    }
    let len = u32::from_le_bytes(
        bytes[bytes.len() - 8..bytes.len() - 4]
            .try_into()
            .map_err(|_| malformed())?,
    ) as usize;
    if len > 1024 * 1024 || len > bytes.len() - 12 {
        return Err(ResultError::LimitExceeded("Parquet footer bytes"));
    }
    let start = bytes.len() - 8 - len;
    let mut reader = Compact::new(&bytes[start..bytes.len() - 8]);
    reader.file_metadata()?;
    if reader.position != len {
        return Err(malformed());
    }
    Ok(start)
}

/// Validate actual page declarations rather than trusting footer size totals.
/// Dictionary decoding uses a buffer capped by max_page_bytes; Arrow decoding
/// begins only after the aggregate expansion bound has been reserved.
pub(super) fn pages(
    bytes: &[u8],
    footer_start: usize,
    metadata: &ParquetMetaData,
    limits: &ResultLimits,
    guard: &RequestGuard<'_>,
) -> Result<(usize, usize), ResultError> {
    let mut ranges = Vec::new();
    let mut total = 0usize;
    let mut expanded = 0usize;
    let mut rows = 0i64;
    for group in metadata.row_groups() {
        guard.check().map_err(ResultError::Interrupted)?;
        if group.num_rows() < 0 {
            return Err(malformed());
        }
        rows = rows.checked_add(group.num_rows()).ok_or_else(malformed)?;
        let mut group_size = 0usize;
        for column in group.columns() {
            let (offset, size) = column.byte_range();
            let start = usize::try_from(offset).map_err(|_| malformed())?;
            let size = usize::try_from(size).map_err(|_| malformed())?;
            let end = start
                .checked_add(size)
                .filter(|end| *end <= footer_start)
                .ok_or_else(malformed)?;
            if start < 4 || column.num_values() < 0 {
                return Err(malformed());
            }
            let mut dictionary_maximum = None;
            let mut cursor = start;
            let mut values = 0i64;
            let mut decoded_size = 0usize;
            while cursor < end {
                guard.check().map_err(ResultError::Interrupted)?;
                let header = page(&bytes[cursor..end], limits)?;
                let page_end = cursor
                    .checked_add(header.header_size)
                    .and_then(|v| v.checked_add(header.compressed))
                    .filter(|v| *v <= end)
                    .ok_or_else(malformed)?;
                // One list row can contain far more than batch_size values.
                // Bound its offsets, validity and fixed-width buffers before Arrow.
                if header.values > 0 && column.column_descr().max_rep_level() > 0 {
                    let per_value = 64usize * (1 + column.column_descr().max_def_level() as usize);
                    expanded = expanded
                        .checked_add(
                            (header.values as usize)
                                .checked_mul(per_value)
                                .ok_or(ResultError::LimitExceeded("decoded expansion"))?,
                        )
                        .filter(|value| *value <= limits.max_decoded_bytes)
                        .ok_or(ResultError::LimitExceeded("decoded expansion"))?;
                }
                if column.column_type() == parquet::basic::Type::BYTE_ARRAY {
                    if header.kind == 2 {
                        if dictionary_maximum.is_some() || !matches!(header.encoding, Some(0 | 2)) {
                            return Err(malformed());
                        }
                        dictionary_maximum = Some(dictionary_maximum_value(
                            &bytes[cursor + header.header_size..page_end],
                            column.compression(),
                            &header,
                        )?);
                    } else if matches!(header.kind, 0 | 3) {
                        let bound = match header.encoding {
                            Some(0) => header.uncompressed,
                            Some(2 | 8) => (header.values as usize)
                                .checked_mul(dictionary_maximum.ok_or_else(malformed)?)
                                .ok_or(ResultError::LimitExceeded("decoded expansion"))?,
                            // Prefix/delta encodings can expand beyond page sizes too.
                            // Athena's standard PLAIN/dictionary encodings are supported.
                            _ => {
                                return Err(ResultError::UnsupportedType(
                                    "Parquet byte-array encoding",
                                ));
                            }
                        };
                        expanded = expanded
                            .checked_add(bound)
                            .filter(|value| *value <= limits.max_decoded_bytes)
                            .ok_or(ResultError::LimitExceeded("decoded expansion"))?;
                    }
                }
                decoded_size = decoded_size
                    .checked_add(header.header_size)
                    .and_then(|v| v.checked_add(header.uncompressed))
                    .ok_or(ResultError::LimitExceeded("uncompressed bytes"))?;
                if decoded_size > limits.max_decoded_bytes {
                    return Err(ResultError::LimitExceeded("uncompressed bytes"));
                }
                values = values.checked_add(header.values).ok_or_else(malformed)?;
                cursor = page_end;
            }
            if cursor != end
                || values != column.num_values()
                || i64::try_from(decoded_size).ok() != Some(column.uncompressed_size())
            {
                return Err(malformed());
            }
            group_size = group_size
                .checked_add(decoded_size)
                .ok_or(ResultError::LimitExceeded("uncompressed bytes"))?;
            ranges.push((start, end));
        }
        if i64::try_from(group_size).ok() != Some(group.total_byte_size()) {
            return Err(malformed());
        }
        total = total
            .checked_add(group_size)
            .filter(|total| *total <= limits.max_decoded_bytes)
            .ok_or(ResultError::LimitExceeded("uncompressed bytes"))?;
    }
    if rows != metadata.file_metadata().num_rows() {
        return Err(malformed());
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(malformed());
    }
    Ok((total, expanded))
}

struct Page {
    kind: i32,
    encoding: Option<i32>,
    dictionary_values: usize,
    header_size: usize,
    compressed: usize,
    uncompressed: usize,
    values: i64,
}

fn dictionary_maximum_value(
    input: &[u8],
    compression: parquet::basic::Compression,
    header: &Page,
) -> Result<usize, ResultError> {
    use std::borrow::Cow;
    // Page declarations have already been capped. Never use decompress_vec,
    // which sizes its allocation from the compressed stream's own header.
    let decoded = match compression {
        parquet::basic::Compression::UNCOMPRESSED => {
            if input.len() != header.uncompressed {
                return Err(malformed());
            }
            Cow::Borrowed(input)
        }
        parquet::basic::Compression::SNAPPY => {
            let mut output = vec![0; header.uncompressed];
            let written = snap::raw::Decoder::new()
                .decompress(input, &mut output)
                .map_err(|_| malformed())?;
            if written != output.len() {
                return Err(malformed());
            }
            Cow::Owned(output)
        }
        _ => return Err(ResultError::UnsupportedType("Parquet compression")),
    };
    let mut input = decoded.as_ref();
    let mut maximum = 0;
    for _ in 0..header.dictionary_values {
        let length = u32::from_le_bytes(
            input
                .get(..4)
                .ok_or_else(malformed)?
                .try_into()
                .map_err(|_| malformed())?,
        ) as usize;
        input = input
            .get(4..)
            .and_then(|bytes| bytes.get(length..))
            .ok_or_else(malformed)?;
        maximum = maximum.max(length);
    }
    if !input.is_empty() {
        return Err(malformed());
    }
    Ok(maximum)
}

fn page(bytes: &[u8], limits: &ResultLimits) -> Result<Page, ResultError> {
    let mut reader = Compact::new(&bytes[..bytes.len().min(MAX_HEADER)]);
    let (mut kind, mut compressed, mut uncompressed) = (None, None, None);
    let (mut v1, mut dictionary, mut v2) = (None, None, None);
    let mut previous = 0;
    let mut seen = 0u16;
    while let Some((id, ty)) = reader.field(&mut previous)? {
        if id <= 8 {
            let bit = 1u16 << id;
            if seen & bit != 0 {
                return Err(malformed());
            }
            seen |= bit;
        }
        match id {
            1 => {
                require_type(ty, 5)?;
                kind = Some(reader.i32()?);
            }
            2 => {
                require_type(ty, 5)?;
                uncompressed = Some(nonnegative(reader.i32()?)?);
            }
            3 => {
                require_type(ty, 5)?;
                compressed = Some(nonnegative(reader.i32()?)?);
            }
            5 | 7 | 8 => {
                require_type(ty, 12)?;
                let info = page_fields(&mut reader)?;
                match id {
                    5 => v1 = Some(info),
                    7 => dictionary = Some(info),
                    8 => v2 = Some(info),
                    _ => unreachable!(),
                }
            }
            _ => reader.value(ty, 0, false)?,
        }
    }
    let (kind, compressed, uncompressed) = (
        kind.ok_or_else(malformed)?,
        compressed.ok_or_else(malformed)?,
        uncompressed.ok_or_else(malformed)?,
    );
    if uncompressed > limits.max_page_bytes || compressed > limits.max_page_bytes {
        return Err(ResultError::LimitExceeded("Parquet page bytes"));
    }
    let info = match kind {
        0 => v1,
        2 => dictionary,
        3 => v2,
        1 => None,
        _ => return Err(malformed()),
    };
    let mut values = 0;
    let mut dictionary_values = 0;
    let mut encoding = None;
    if kind != 1 {
        let info = info.ok_or_else(malformed)?;
        let count = info[0].ok_or_else(malformed)?;
        if count < 0 || count as usize > (limits.max_page_bytes / 8).max(1) {
            return Err(ResultError::LimitExceeded("Parquet page values"));
        }
        encoding = Some(info[if kind == 3 { 3 } else { 1 }].ok_or_else(malformed)?);
        if kind != 2 {
            values = count as i64;
        } else {
            dictionary_values = count as usize;
        }
        if kind == 3 {
            let nulls = info[1].ok_or_else(malformed)?;
            let rows = info[2].ok_or_else(malformed)?;
            let definitions = nonnegative(info[4].ok_or_else(malformed)?)?;
            let repetitions = nonnegative(info[5].ok_or_else(malformed)?)?;
            if nulls < 0
                || nulls > count
                || rows < 0
                || rows > count
                || definitions
                    .checked_add(repetitions)
                    .is_none_or(|n| n > compressed || n > uncompressed)
            {
                return Err(malformed());
            }
        }
    }
    Ok(Page {
        kind,
        encoding,
        dictionary_values,
        header_size: reader.position,
        compressed,
        uncompressed,
        values,
    })
}

fn page_fields(reader: &mut Compact<'_>) -> Result<[Option<i32>; 6], ResultError> {
    let mut result = [None; 6];
    let mut previous = 0;
    while let Some((id, ty)) = reader.field(&mut previous)? {
        if id <= 6 && ty == 5 {
            let entry = &mut result[(id - 1) as usize];
            if entry.is_some() {
                return Err(malformed());
            }
            *entry = Some(reader.i32()?);
        } else {
            reader.value(ty, 1, false)?;
        }
    }
    Ok(result)
}

fn nonnegative(value: i32) -> Result<usize, ResultError> {
    usize::try_from(value).map_err(|_| malformed())
}
fn require_type(actual: u8, expected: u8) -> Result<(), ResultError> {
    if actual == expected {
        Ok(())
    } else {
        Err(malformed())
    }
}

/// Compact Thrift framing only: no strings, vectors or decoded values allocated.
struct Compact<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Compact<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn byte(&mut self) -> Result<u8, ResultError> {
        let byte = *self.bytes.get(self.position).ok_or_else(malformed)?;
        self.position += 1;
        Ok(byte)
    }
    fn advance(&mut self, length: usize) -> Result<(), ResultError> {
        self.position = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(malformed)?;
        Ok(())
    }
    fn unsigned(&mut self, bits: u32) -> Result<u64, ResultError> {
        let mut value = 0u64;
        for shift in (0..bits).step_by(7) {
            let byte = self.byte()?;
            let payload = (byte & 127) as u64;
            if payload > (u64::MAX >> (64 - bits + shift)) {
                return Err(malformed());
            }
            value |= payload << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(malformed())
    }
    fn i32(&mut self) -> Result<i32, ResultError> {
        let value = self.unsigned(32)? as u32;
        Ok(((value >> 1) as i32) ^ -((value & 1) as i32))
    }
    fn field(&mut self, previous: &mut i16) -> Result<Option<(i16, u8)>, ResultError> {
        let header = self.byte()?;
        if header == 0 {
            return Ok(None);
        }
        let ty = header & 15;
        if !(1..=12).contains(&ty) {
            return Err(malformed());
        }
        let delta = (header >> 4) as i16;
        let id = if delta == 0 {
            let value = self.unsigned(16)? as u16;
            ((value >> 1) as i16) ^ -((value & 1) as i16)
        } else {
            previous.checked_add(delta).ok_or_else(malformed)?
        };
        if id <= 0 {
            return Err(malformed());
        }
        *previous = id;
        Ok(Some((id, ty)))
    }
    // Parquet encodes the logical schema as a flat list. Compact nesting alone
    // does not bound the recursion or child-vector allocations in its converter.
    fn file_metadata(&mut self) -> Result<(), ResultError> {
        let mut previous = 0;
        let mut schema = false;
        while let Some((id, ty)) = self.field(&mut previous)? {
            if id == 2 {
                if schema {
                    return Err(malformed());
                }
                schema = true;
                require_type(ty, 9)?;
                self.schema()?;
            } else {
                self.value(ty, 0, false)?;
            }
        }
        if !schema {
            return Err(malformed());
        }
        Ok(())
    }
    fn schema(&mut self) -> Result<(), ResultError> {
        let header = self.byte()?;
        require_type(header & 15, 12)?;
        let count = if header >> 4 == 15 {
            self.unsigned(32)? as usize
        } else {
            (header >> 4) as usize
        };
        if count == 0 || count > 4096 || count > self.bytes.len() - self.position {
            return Err(ResultError::LimitExceeded("Parquet schema elements"));
        }
        let mut remaining = [0usize; MAX_DEPTH + 1];
        let mut depth = 0;
        for index in 0..count {
            if index != 0 {
                while depth > 0 && remaining[depth] == 0 {
                    depth -= 1;
                }
                if depth == 0 {
                    return Err(malformed());
                }
                remaining[depth] -= 1;
            }
            let mut previous = 0;
            let mut children = None;
            let mut physical = false;
            while let Some((id, ty)) = self.field(&mut previous)? {
                match id {
                    1 => {
                        require_type(ty, 5)?;
                        self.i32()?;
                        physical = true;
                    }
                    2 => {
                        require_type(ty, 5)?;
                        let width = nonnegative(self.i32()?)?;
                        // Athena's fixed-width decimal payloads need at most 16
                        // bytes. Permit up to decimal256, never arbitrary i32 widths.
                        if width == 0 || width > 32 {
                            return Err(ResultError::UnsupportedType(
                                "Parquet fixed width exceeds 32 bytes",
                            ));
                        }
                    }
                    5 => {
                        require_type(ty, 5)?;
                        if children.is_some() {
                            return Err(malformed());
                        }
                        children = Some(nonnegative(self.i32()?)?);
                    }
                    _ => self.value(ty, 1, false)?,
                }
            }
            let children = children.unwrap_or(0);
            if children > count - index - 1 || (children > 0 && physical) {
                return Err(malformed());
            }
            if children > 0 {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(ResultError::LimitExceeded("Parquet schema depth"));
                }
                remaining[depth] = children;
            } else if index == 0 || !physical {
                return Err(malformed());
            }
        }
        if remaining.iter().any(|count| *count != 0) {
            return Err(malformed());
        }
        Ok(())
    }
    fn structure(&mut self, depth: usize) -> Result<(), ResultError> {
        if depth > MAX_DEPTH {
            return Err(malformed());
        }
        let mut previous = 0;
        while let Some((_, ty)) = self.field(&mut previous)? {
            self.value(ty, depth, false)?;
        }
        Ok(())
    }
    fn value(&mut self, ty: u8, depth: usize, element: bool) -> Result<(), ResultError> {
        if depth > MAX_DEPTH {
            return Err(malformed());
        }
        match ty {
            1 | 2 => {
                if element && !matches!(self.byte()?, 1 | 2) {
                    return Err(malformed());
                }
            }
            3 => self.advance(1)?,
            4 => {
                self.unsigned(16)?;
            }
            5 => {
                self.unsigned(32)?;
            }
            6 => {
                self.unsigned(64)?;
            }
            7 => self.advance(8)?,
            8 => {
                let n = self.unsigned(32)? as usize;
                self.advance(n)?;
            }
            9 | 10 => {
                let header = self.byte()?;
                let count = if header >> 4 == 15 {
                    self.unsigned(32)? as usize
                } else {
                    (header >> 4) as usize
                };
                let item = header & 15;
                if count > self.bytes.len() - self.position || !(1..=12).contains(&item) {
                    return Err(malformed());
                }
                for _ in 0..count {
                    self.value(item, depth + 1, true)?;
                }
            }
            11 => {
                let count = self.unsigned(32)? as usize;
                if count != 0 {
                    let types = self.byte()?;
                    if count > (self.bytes.len() - self.position) / 2 {
                        return Err(malformed());
                    }
                    for _ in 0..count {
                        self.value(types >> 4, depth + 1, true)?;
                        self.value(types & 15, depth + 1, true)?;
                    }
                }
            }
            12 => self.structure(depth + 1)?,
            _ => return Err(malformed()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{BooleanArray, Int32Array, RecordBatch};
    use aws_credential_types::{Credentials, provider::SharedCredentialsProvider};
    use parquet::arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder};
    use std::sync::Arc;
    fn append_varint(output: &mut Vec<u8>, mut value: u32) {
        while value >= 128 {
            output.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        output.push(value as u8);
    }
    fn malicious_schema(children: i32, groups: usize, width: Option<i32>) -> Vec<u8> {
        let mut metadata = vec![0x15, 2, 0x19, 0xfc]; // version, list<SchemaElement>
        append_varint(&mut metadata, (groups + 2) as u32);
        for index in 0..=groups {
            if index == 0 {
                metadata.extend_from_slice(&[0x48, 1, b'g', 0x15]);
            } else {
                metadata.extend_from_slice(&[0x35, 0, 0x18, 1, b'g', 0x15]);
            } // required non-root group, name, children
            let count = if index == 0 { children } else { 1 };
            append_varint(&mut metadata, (count as u32) << 1);
            metadata.push(0);
        }
        metadata.extend_from_slice(&[0x15, if width.is_some() { 14 } else { 2 }]);
        if let Some(width) = width {
            metadata.push(0x15);
            append_varint(&mut metadata, (width as u32) << 1);
            metadata.extend_from_slice(&[0x15, 0, 0x18, 1, b'v', 0]);
        } else {
            metadata.extend_from_slice(&[0x25, 0, 0x18, 1, b'v', 0]);
        }
        metadata.extend_from_slice(&[0x16, 0, 0x19, 0x0c, 0]); // rows, no row groups
        let mut file = b"PAR1".to_vec();
        file.extend_from_slice(&metadata);
        file.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        file.extend_from_slice(b"PAR1");
        file
    }
    #[tokio::test]
    async fn rejects_hostile_schema_counts_depth_and_width_before_library_conversion() {
        let config = aws_types::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-west-2"))
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "TEST",
                "synthetic",
                None,
                None,
                "test",
            )))
            .http_client(aws_smithy_http_client::test_util::NeverClient::new())
            .build();
        let connection = crate::Connection::from_config(
            crate::ConnectionOptions::new("synthetic", "schema"),
            config,
        )
        .await
        .unwrap();
        let token = crate::QueryCancellation::new();
        let guard = RequestGuard {
            connection: &connection,
            cancellation: &token,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        for (file, expected) in [
            (malicious_schema(i32::MAX, 0, None), malformed()),
            (
                malicious_schema(1, 40, None),
                ResultError::LimitExceeded("Parquet schema depth"),
            ),
            (
                malicious_schema(1, 0, Some(i32::MAX)),
                ResultError::UnsupportedType("Parquet fixed width exceeds 32 bytes"),
            ),
        ] {
            let actual = crate::result::decode::decode(
                file.into(),
                "presence",
                &ResultLimits::default(),
                &crate::result::Budget::default(),
                &guard,
            )
            .err()
            .expect("hostile schema was accepted");
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn rejects_oversized_page_before_any_decompressor_is_constructed() {
        // PageHeader(DATA_PAGE, 65536 bytes decoded, 1 byte compressed), v1 header.
        let bytes = [
            0x15, 0, 0x15, 0x80, 0x80, 8, 0x15, 2, 0x2c, 0x15, 2, 0x15, 0, 0x15, 0, 0x15, 0, 0, 0,
            0,
        ];
        let limits = ResultLimits {
            max_page_bytes: 1024,
            ..ResultLimits::default()
        };
        assert!(matches!(
            page(&bytes, &limits),
            Err(ResultError::LimitExceeded("Parquet page bytes"))
        ));
    }
    #[test]
    fn rejects_truncated_overflowing_deep_and_huge_container_metadata() {
        for input in [
            vec![0x1b, 0xff, 0xff, 0xff, 0xff, 15],
            vec![0x15, 0x80, 0x80, 0x80, 0x80, 0x10],
            vec![0x19, 0xf5, 0xff, 0xff, 0xff, 0xff, 15],
            vec![0x1c; 40],
        ] {
            assert!(Compact::new(&input).structure(0).is_err());
        }
    }
    #[test]
    fn permits_bool_fields_lists_and_unknown_optional_values_without_allocating() {
        // bool field, list<bool>[true,false], nested empty struct, STOP.
        let input = [0x11, 0x19, 0x21, 1, 2, 0x1c, 0, 0];
        let mut reader = Compact::new(&input);
        reader.structure(0).unwrap();
        assert_eq!(reader.position, input.len());
    }

    #[tokio::test]
    async fn dictionary_expansion_is_bounded_before_arrow_copies_values() {
        let batch = RecordBatch::try_from_iter(vec![
            (
                "v",
                Arc::new(arrow_array::StringArray::from(vec!["x".repeat(512); 64])) as _,
            ),
            (
                "presence",
                Arc::new(BooleanArray::from(vec![true; 64])) as _,
            ),
        ])
        .unwrap();
        let mut bytes = Vec::new();
        let properties = parquet::file::properties::WriterProperties::builder()
            .set_compression(parquet::basic::Compression::SNAPPY)
            .build();
        let mut writer =
            ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        let config = aws_types::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-west-2"))
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "TEST",
                "synthetic",
                None,
                None,
                "test",
            )))
            .http_client(aws_smithy_http_client::test_util::NeverClient::new())
            .build();
        let connection = crate::Connection::from_config(
            crate::ConnectionOptions::new("synthetic", "schema"),
            config,
        )
        .await
        .unwrap();
        let token = crate::QueryCancellation::new();
        let guard = RequestGuard {
            connection: &connection,
            cancellation: &token,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        let limits = ResultLimits {
            max_decoded_bytes: 2048,
            ..ResultLimits::default()
        };
        let result = crate::result::decode::decode(
            bytes.clone().into(),
            "presence",
            &limits,
            &crate::result::Budget::default(),
            &guard,
        );
        assert_eq!(
            result.err(),
            Some(ResultError::LimitExceeded("decoded expansion"))
        );
        let limits = ResultLimits {
            max_decoded_bytes: 65536,
            ..ResultLimits::default()
        };
        let result = crate::result::decode::decode(
            bytes.into(),
            "presence",
            &limits,
            &crate::result::Budget::default(),
            &guard,
        )
        .unwrap();
        assert_eq!(
            result
                .batches
                .iter()
                .map(RecordBatch::num_rows)
                .sum::<usize>(),
            64
        );
    }

    #[tokio::test]
    async fn forged_snappy_page_is_rejected_through_the_decoder_before_allocation() {
        let batch = RecordBatch::try_from_iter(vec![
            ("v", Arc::new(Int32Array::from(vec![1, 2])) as _),
            (
                "presence",
                Arc::new(BooleanArray::from(vec![true, true])) as _,
            ),
        ])
        .unwrap();
        let mut bytes = Vec::new();
        let properties = parquet::file::properties::WriterProperties::builder()
            .set_compression(parquet::basic::Compression::SNAPPY)
            .build();
        let mut writer =
            ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(&bytes))
                .unwrap();
        let start = builder.metadata().row_group(0).column(0).byte_range().0 as usize;
        let mut reader = Compact::new(&bytes[start..]);
        let mut previous = 0;
        let mut size_byte = None;
        while let Some((id, ty)) = reader.field(&mut previous).unwrap() {
            if id == 2 {
                let position = reader.position;
                reader.i32().unwrap();
                assert_eq!(reader.position - position, 1);
                size_byte = Some(start + position);
                break;
            }
            reader.value(ty, 0, false).unwrap();
        }
        // Same header width: change only the decoded-page declaration to63bytes.
        // Footer totals and compressed payload remain small and unchanged.
        bytes[size_byte.unwrap()] = 126;
        let config = aws_types::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-west-2"))
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "TEST",
                "synthetic",
                None,
                None,
                "test",
            )))
            .http_client(aws_smithy_http_client::test_util::NeverClient::new())
            .build();
        let connection = crate::Connection::from_config(
            crate::ConnectionOptions::new("synthetic", "schema"),
            config,
        )
        .await
        .unwrap();
        let token = crate::QueryCancellation::new();
        let guard = RequestGuard {
            connection: &connection,
            cancellation: &token,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        let limits = ResultLimits {
            max_page_bytes: 32,
            ..ResultLimits::default()
        };
        let result = crate::result::decode::decode(
            bytes.into(),
            "presence",
            &limits,
            &crate::result::Budget::default(),
            &guard,
        );
        assert!(matches!(
            result,
            Err(ResultError::LimitExceeded("Parquet page bytes"))
        ));
    }
}
