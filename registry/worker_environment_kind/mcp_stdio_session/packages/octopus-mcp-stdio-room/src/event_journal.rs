use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub(crate) struct StdioEventJournal {
    path: PathBuf,
    segments: Vec<Segment>,
    segment_max_bytes: u64,
    retained_segments: usize,
    sequence: u64,
}

struct Segment {
    path: PathBuf,
    start: u64,
    end: u64,
    bytes: u64,
    offsets: Vec<u64>,
}

impl Segment {
    fn empty(path: PathBuf, start: u64) -> Self {
        Self {
            path,
            start,
            end: start.saturating_sub(1),
            bytes: 0,
            offsets: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

impl StdioEventJournal {
    pub(crate) fn open(
        path: PathBuf,
        segment_max_bytes: u64,
        retained_segments: usize,
    ) -> Result<Self, String> {
        if segment_max_bytes == 0 {
            return Err("MCP stdio event segment byte ceiling is zero".to_string());
        }
        if retained_segments < 2 {
            return Err("MCP stdio event journal must retain at least two segments".to_string());
        }
        let parent = path
            .parent()
            .ok_or_else(|| format!("MCP stdio event log '{}' has no parent", path.display()))?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let mut archived = archived_segments(&path)?;
        archived.sort_by_key(|(start, end, _)| (*start, *end));
        let mut segments = Vec::new();
        let mut expected = None;
        for (declared_start, declared_end, segment_path) in archived {
            let segment = scan_segment(segment_path, segment_max_bytes)?;
            if segment.is_empty() || segment.start != declared_start || segment.end != declared_end
            {
                return Err(format!(
                    "MCP stdio event archive '{}' disagrees with declared range {declared_start}..={declared_end}",
                    segment.path.display()
                ));
            }
            admit_segment(&segment, &mut expected)?;
            segments.push(segment);
        }
        let active = if path.exists() {
            let segment = scan_segment(path.clone(), segment_max_bytes)?;
            if segment.is_empty() {
                Segment::empty(path.clone(), expected.unwrap_or(1))
            } else {
                admit_segment(&segment, &mut expected)?;
                segment
            }
        } else {
            create_segment(&path)?;
            Segment::empty(path.clone(), expected.unwrap_or(1))
        };
        segments.push(active);
        let sequence = segments
            .iter()
            .rev()
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.end)
            .unwrap_or(0);
        let mut journal = Self {
            path,
            segments,
            segment_max_bytes,
            retained_segments,
            sequence,
        };
        journal.prune()?;
        Ok(journal)
    }

    pub(crate) fn append(&mut self, event: Value) -> Result<(), String> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "MCP stdio event sequence overflowed".to_string())?;
        let row = json!({ "seq": sequence, "body": event });
        let mut encoded = serde_json::to_vec(&row)
            .map_err(|error| format!("MCP stdio event did not serialize: {error}"))?;
        encoded.push(b'\n');
        let encoded_len = u64::try_from(encoded.len())
            .map_err(|_| "MCP stdio event row length exceeds u64".to_string())?;
        if encoded_len > self.segment_max_bytes {
            return Err(format!(
                "MCP stdio event row requires {encoded_len} bytes, exceeding segment ceiling {}",
                self.segment_max_bytes
            ));
        }
        let active = self
            .segments
            .last()
            .ok_or_else(|| "MCP stdio event journal has no active segment".to_string())?;
        let next_bytes = active
            .bytes
            .checked_add(encoded_len)
            .ok_or_else(|| "MCP stdio event segment length overflowed".to_string())?;
        if !active.is_empty() && next_bytes > self.segment_max_bytes {
            self.rotate()?;
            self.prune()?;
        }
        let active = self
            .segments
            .last_mut()
            .ok_or_else(|| "MCP stdio event journal has no active segment".to_string())?;
        let was_empty = active.is_empty();
        let offset = active.bytes;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|error| error.to_string())?;
        file.write_all(&encoded)
            .map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())?;
        active.offsets.push(offset);
        active.bytes = active
            .bytes
            .checked_add(encoded_len)
            .ok_or_else(|| "MCP stdio event segment length overflowed".to_string())?;
        if was_empty {
            active.start = sequence;
        }
        active.end = sequence;
        self.sequence = sequence;
        Ok(())
    }

    pub(crate) fn read_after(&self, cursor: u64, limit: usize) -> Result<Vec<Value>, String> {
        if cursor > self.sequence {
            return Err(format!(
                "MCP stdio event cursor {cursor} is ahead of durable cursor {}",
                self.sequence
            ));
        }
        if limit == 0 || cursor == self.sequence {
            return Ok(Vec::new());
        }
        let earliest = self
            .earliest_cursor()
            .ok_or_else(|| "MCP stdio event cursor exists without retained rows".to_string())?;
        let next = cursor
            .checked_add(1)
            .ok_or_else(|| "MCP stdio event cursor overflowed".to_string())?;
        if next < earliest {
            return Err(format!(
                "MCP stdio event cursor {cursor} fell behind retained history; earliest available event is {earliest}, latest is {}",
                self.sequence
            ));
        }
        let mut rows = Vec::with_capacity(limit.min(256));
        for segment in &self.segments {
            if segment.is_empty() || segment.end < next {
                continue;
            }
            read_segment(segment, next, limit, self.segment_max_bytes, &mut rows)?;
            if rows.len() == limit {
                break;
            }
        }
        Ok(rows)
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn earliest_cursor(&self) -> Option<u64> {
        self.segments
            .iter()
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.start)
    }

    fn rotate(&mut self) -> Result<(), String> {
        let active = self
            .segments
            .last()
            .ok_or_else(|| "MCP stdio event journal has no active segment".to_string())?;
        if active.is_empty() {
            return Ok(());
        }
        let archive = archive_path(&self.path, active.start, active.end)?;
        if archive.exists() {
            return Err(format!(
                "MCP stdio event archive '{}' already exists",
                archive.display()
            ));
        }
        std::fs::rename(&self.path, &archive).map_err(|error| error.to_string())?;
        if let Err(error) = create_segment(&self.path) {
            let _ = std::fs::rename(&archive, &self.path);
            return Err(error);
        }
        self.segments
            .last_mut()
            .ok_or_else(|| "MCP stdio event journal has no active segment".to_string())?
            .path = archive;
        self.segments.push(Segment::empty(
            self.path.clone(),
            self.sequence
                .checked_add(1)
                .ok_or_else(|| "MCP stdio event sequence overflowed".to_string())?,
        ));
        sync_parent(&self.path)
    }

    fn prune(&mut self) -> Result<(), String> {
        while self.segments.len() > self.retained_segments {
            if self.segments[0].path == self.path {
                return Err("MCP stdio event retention selected the active segment".to_string());
            }
            let path = self.segments[0].path.clone();
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
            self.segments.remove(0);
            sync_parent(&self.path)?;
        }
        Ok(())
    }
}

fn scan_segment(path: PathBuf, segment_max_bytes: u64) -> Result<Segment, String> {
    let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
    let bytes = file.metadata().map_err(|error| error.to_string())?.len();
    if bytes > segment_max_bytes {
        return Err(format!(
            "MCP stdio event segment '{}' has {bytes} bytes, exceeding segment ceiling {segment_max_bytes}",
            path.display()
        ));
    }
    let mut reader = BufReader::new(file);
    let mut offsets = Vec::new();
    let mut position = 0u64;
    let mut start = None;
    let mut end: Option<u64> = None;
    loop {
        let Some(mut line) = read_bounded_row(&mut reader, segment_max_bytes, &path)? else {
            break;
        };
        let read = line.len();
        if line.last() != Some(&b'\n') {
            return Err(format!(
                "MCP stdio event segment '{}' ends with a partial row",
                path.display()
            ));
        }
        line.pop();
        let row: Value = serde_json::from_slice(&line).map_err(|error| {
            format!(
                "MCP stdio event segment '{}' row {} is malformed: {error}",
                path.display(),
                offsets.len() + 1
            )
        })?;
        let sequence = validate_row(&row)?;
        if let Some(previous) = end {
            let expected = previous
                .checked_add(1)
                .ok_or_else(|| "MCP stdio event sequence overflowed".to_string())?;
            if sequence != expected {
                return Err(format!(
                    "MCP stdio event segment '{}' sequence {sequence} is non-contiguous; expected {expected}",
                    path.display()
                ));
            }
        }
        offsets.push(position);
        position = position
            .checked_add(
                u64::try_from(read)
                    .map_err(|_| "MCP stdio event segment row length exceeds u64".to_string())?,
            )
            .ok_or_else(|| "MCP stdio event segment length overflowed".to_string())?;
        start.get_or_insert(sequence);
        end = Some(sequence);
    }
    if position != bytes {
        return Err(format!(
            "MCP stdio event segment '{}' changed while it was indexed",
            path.display()
        ));
    }
    let start = start.unwrap_or(1);
    Ok(Segment {
        path,
        start,
        end: end.unwrap_or(start.saturating_sub(1)),
        bytes,
        offsets,
    })
}

fn validate_row(row: &Value) -> Result<u64, String> {
    let object = row
        .as_object()
        .ok_or_else(|| "MCP stdio event row is not an object".to_string())?;
    if object.len() != 2 || !object.contains_key("body") {
        return Err("MCP stdio event row violates the cursor contract".to_string());
    }
    object
        .get("seq")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| "MCP stdio event row has no positive seq".to_string())
}

fn admit_segment(segment: &Segment, expected: &mut Option<u64>) -> Result<(), String> {
    if let Some(expected) = expected {
        if segment.start != *expected {
            return Err(format!(
                "MCP stdio event segment '{}' starts at {}, expected {}",
                segment.path.display(),
                segment.start,
                expected
            ));
        }
    }
    *expected = Some(
        segment
            .end
            .checked_add(1)
            .ok_or_else(|| "MCP stdio event sequence overflowed".to_string())?,
    );
    Ok(())
}

fn read_segment(
    segment: &Segment,
    cursor: u64,
    limit: usize,
    row_max_bytes: u64,
    rows: &mut Vec<Value>,
) -> Result<(), String> {
    if std::fs::metadata(&segment.path)
        .map_err(|error| error.to_string())?
        .len()
        != segment.bytes
    {
        return Err(format!(
            "MCP stdio event segment '{}' changed after indexing",
            segment.path.display()
        ));
    }
    let first = cursor.max(segment.start);
    let index = usize::try_from(first - segment.start)
        .map_err(|_| "MCP stdio event index exceeds usize".to_string())?;
    let Some(offset) = segment.offsets.get(index).copied() else {
        return Ok(());
    };
    let mut file = std::fs::File::open(&segment.path).map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(file);
    let mut expected = first;
    while rows.len() < limit && expected <= segment.end {
        let Some(mut line) = read_bounded_row(&mut reader, row_max_bytes, &segment.path)? else {
            return Err(format!(
                "MCP stdio event segment '{}' changed while reading",
                segment.path.display()
            ));
        };
        if line.last() != Some(&b'\n') {
            return Err(format!(
                "MCP stdio event segment '{}' changed while reading",
                segment.path.display()
            ));
        }
        line.pop();
        let row: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
        let sequence = validate_row(&row)?;
        if sequence != expected {
            return Err(format!(
                "MCP stdio event segment '{}' indexed sequence changed from {expected} to {sequence}",
                segment.path.display()
            ));
        }
        rows.push(row);
        expected = expected
            .checked_add(1)
            .ok_or_else(|| "MCP stdio event sequence overflowed".to_string())?;
    }
    Ok(())
}

fn read_bounded_row<R: BufRead>(
    reader: &mut R,
    max_bytes: u64,
    path: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let current = u64::try_from(line.len())
            .map_err(|_| "MCP stdio event row length exceeds u64".to_string())?;
        let consumed_u64 = u64::try_from(consumed)
            .map_err(|_| "MCP stdio event row length exceeds u64".to_string())?;
        let next = current
            .checked_add(consumed_u64)
            .ok_or_else(|| "MCP stdio event row length overflowed".to_string())?;
        if next > max_bytes {
            return Err(format!(
                "MCP stdio event segment '{}' row exceeds segment ceiling {max_bytes}",
                path.display()
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn archived_segments(path: &Path) -> Result<Vec<(u64, u64, PathBuf)>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("MCP stdio event log '{}' has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("MCP stdio event log '{}' has invalid name", path.display()))?;
    let stem = name
        .strip_suffix(".jsonl")
        .ok_or_else(|| "MCP stdio event log must be a .jsonl file".to_string())?;
    let prefix = format!("{stem}.");
    let mut segments = Vec::new();
    for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_name = entry.file_name();
        let Some(range) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|name| name.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let (start, end) = range
            .split_once('-')
            .ok_or_else(|| format!("MCP stdio event archive '{range}' has no cursor range"))?;
        let start = start.parse::<u64>().map_err(|error| error.to_string())?;
        let end = end.parse::<u64>().map_err(|error| error.to_string())?;
        if start == 0 || end < start {
            return Err(format!(
                "MCP stdio event archive '{range}' has invalid cursor range"
            ));
        }
        segments.push((start, end, entry.path()));
    }
    Ok(segments)
}

fn archive_path(path: &Path, start: u64, end: u64) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("MCP stdio event log '{}' has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("MCP stdio event log '{}' has invalid name", path.display()))?;
    let stem = name
        .strip_suffix(".jsonl")
        .ok_or_else(|| "MCP stdio event log must be a .jsonl file".to_string())?;
    Ok(parent.join(format!("{stem}.{start:020}-{end:020}.jsonl")))
}

fn create_segment(path: &Path) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_data())
        .map_err(|error| error.to_string())
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("MCP stdio event log '{}' has no parent", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{read_bounded_row, StdioEventJournal};
    use serde_json::json;
    use std::io::BufReader;
    use std::path::Path;

    #[test]
    fn retained_segments_report_a_cursor_gap() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("server_events.jsonl");
        let mut journal = StdioEventJournal::open(path.clone(), 256, 2).unwrap();
        for index in 0..6 {
            journal
                .append(json!({ "index": index, "payload": "x".repeat(120) }))
                .unwrap();
        }
        assert!(journal.earliest_cursor().unwrap() > 1);
        assert!(journal
            .read_after(0, 10)
            .unwrap_err()
            .contains("fell behind"));
        let sequence = journal.sequence();
        drop(journal);
        assert_eq!(
            StdioEventJournal::open(path, 256, 2).unwrap().sequence(),
            sequence
        );
    }

    #[test]
    fn restart_rejects_segment_metadata_over_the_declared_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("server_events.jsonl");
        std::fs::write(&path, vec![b'x'; 257]).unwrap();
        let error = match StdioEventJournal::open(path, 256, 2) {
            Ok(_) => panic!("oversized segment was admitted"),
            Err(error) => error,
        };
        assert!(error.contains("exceeding segment ceiling 256"));
    }

    #[test]
    fn row_reader_stops_at_the_declared_ceiling() {
        let path = Path::new("oversized-event-row.jsonl");
        let mut reader = BufReader::new(std::io::Cursor::new(vec![b'x'; 257]));
        let error = read_bounded_row(&mut reader, 256, path).unwrap_err();
        assert!(error.contains("row exceeds segment ceiling 256"));
    }
}
