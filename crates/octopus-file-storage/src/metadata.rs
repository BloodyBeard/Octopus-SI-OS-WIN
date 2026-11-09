use std::time::SystemTime;
#[cfg(unix)]
use std::time::{Duration, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: [u64; 7],
}

impl FileStamp {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                [
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime() as u64,
                    metadata.ctime_nsec() as u64,
                    metadata.mode() as u64,
                    metadata.uid() as u64,
                    metadata.gid() as u64,
                ]
            },
        }
    }

    pub fn is_settled(&self, observed_before_read: SystemTime) -> bool {
        #[cfg(unix)]
        {
            let [_, _, seconds, nanos, ..] = self.unix;
            if nanos >= 1_000_000_000 {
                return false;
            }
            let changed = UNIX_EPOCH.checked_add(Duration::new(seconds, nanos as u32));
            [self.modified, changed].into_iter().all(|time| {
                time.and_then(|time| observed_before_read.duration_since(time).ok())
                    .is_some_and(|age| age >= Duration::from_secs(1))
            })
        }
        #[cfg(not(unix))]
        {
            let _ = observed_before_read;
            false
        }
    }
}
