use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

const COMPRESSED_SUFFIX: &str = ".zst";
const MAX_NOT_FOUND_RETRIES: usize = 3;
const OPEN_ROLLOUT_LINE_READER_RETRY_DELAY: Duration = Duration::from_millis(50);

pub async fn open_rollout_line_reader(path: &Path) -> io::Result<RolloutLineReader> {
    for _ in 0..MAX_NOT_FOUND_RETRIES {
        match open_once(path).await {
            Ok(reader) => return Ok(reader),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tokio::time::sleep(OPEN_ROLLOUT_LINE_READER_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }
    open_once(path).await
}

async fn open_once(path: &Path) -> io::Result<RolloutLineReader> {
    let path = existing_rollout_path(path)
        .await
        .unwrap_or_else(|| path.to_path_buf());
    if is_compressed_rollout_path(path.as_path()) {
        let reader = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            let input = File::open(path.as_path())?;
            let decoder = zstd::stream::read::Decoder::new(input)?;
            Ok::<_, io::Error>(
                io::BufReader::new(Box::new(decoder) as Box<dyn Read + Send>).lines(),
            )
        })
        .await
        .map_err(io::Error::other)??;
        return Ok(RolloutLineReader {
            inner: RolloutLineReaderInner::Blocking(Some(reader)),
        });
    }
    let file = tokio::fs::File::open(path).await?;
    Ok(RolloutLineReader {
        inner: RolloutLineReaderInner::Plain(tokio::io::BufReader::new(file).lines()),
    })
}

pub struct RolloutLineReader {
    inner: RolloutLineReaderInner,
}

type BlockingLineReader = std::io::Lines<std::io::BufReader<Box<dyn Read + Send>>>;

enum RolloutLineReaderInner {
    Plain(tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>),
    Blocking(Option<BlockingLineReader>),
}

impl RolloutLineReader {
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
        match &mut self.inner {
            RolloutLineReaderInner::Plain(lines) => lines.next_line().await,
            RolloutLineReaderInner::Blocking(slot) => {
                let Some(mut reader) = slot.take() else {
                    return Err(io::Error::other("compressed rollout reader is busy"));
                };
                let (line, reader) =
                    tokio::task::spawn_blocking(move || (reader.next().transpose(), reader))
                        .await
                        .map_err(io::Error::other)?;
                *slot = Some(reader);
                line
            }
        }
    }
}

fn compressed_rollout_path(path: &Path) -> PathBuf {
    if is_compressed_rollout_path(path) {
        return path.to_path_buf();
    }
    let mut file_name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| OsStr::new("rollout.jsonl").to_os_string());
    file_name.push(COMPRESSED_SUFFIX);
    path.with_file_name(file_name)
}

fn plain_rollout_path(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return path.to_path_buf();
    };
    let Some(plain_file_name) = file_name.strip_suffix(COMPRESSED_SUFFIX) else {
        return path.to_path_buf();
    };
    path.with_file_name(plain_file_name)
}

fn is_compressed_rollout_path(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(".jsonl.zst"))
}

async fn existing_rollout_path(path: &Path) -> Option<PathBuf> {
    let plain_path = plain_rollout_path(path);
    if matches!(tokio::fs::metadata(plain_path.as_path()).await, Ok(metadata) if metadata.is_file())
    {
        return Some(plain_path);
    }
    let compressed_path = compressed_rollout_path(plain_path.as_path());
    if matches!(tokio::fs::metadata(compressed_path.as_path()).await, Ok(metadata) if metadata.is_file())
    {
        return Some(compressed_path);
    }
    None
}
